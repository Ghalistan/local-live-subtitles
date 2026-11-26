import { fetchElevenLabsModelsViaServer, sanitizeElevenLabsModels } from '../lib/elevenlabs';
import {
  DEFAULT_GOOGLE_TARGET_LANGUAGE,
  DEFAULT_TRANSLATOR_TARGET_LANGUAGE,
  googleModelSupportsDirectS2tt,
  pickDefaultGoogleModel,
  sanitizeGoogleModels,
  sanitizeGoogleTargetLanguage,
  sanitizeTranslatorTargetLanguage,
} from '../lib/gemini';
import {
  CLOUD_PROVIDERS,
  DEFAULT_DIARIZATION_MODEL,
  DEFAULT_ENDPOINT_MECHANISM,
  DEFAULT_SUBTITLE_STYLE,
  DEFAULT_TRANSLATOR_ENGINE,
  DEFAULT_VAD_MODEL,
  LINE_EXPIRE_MS,
  parsePortFromServerUrl,
  portToServerUrl,
  pushSubtitleLine,
  sanitizeDiarizationModel,
  sanitizeEndpointMechanism,
  sanitizeProvider,
  sanitizeSubtitleStyle,
  sanitizeTranslatorEngine,
  sanitizeVadModel,
  type BackgroundToOffscreenMessage,
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
  findModelEntry,
  loadModelVariants,
  migrateLegacyModelSelection,
  resolveModelSource,
  sanitizeCustomModels,
  sanitizeHiddenModelIds,
  sanitizeTargetLanguage,
} from '../lib/models';

export default defineBackground(() => {
  const state: ExtensionState = {
    status: 'idle',
    provider: 'local',
    tabId: null,
    modelId: DEFAULT_MODEL_ID,
    quantId: DEFAULT_QUANT_ID,
    hiddenModelIds: [],
    customModels: [],
    geminiApiKey: '',
    speechApiKeys: { soniox: '', elevenlabs: '' },
    googleModelId: '',
    googleModels: [],
    elevenLabsModelId: '',
    elevenLabsModels: [],
    googleTargetLanguage: DEFAULT_GOOGLE_TARGET_LANGUAGE,
    translatorEngine: DEFAULT_TRANSLATOR_ENGINE,
    translatorTargetLanguage: DEFAULT_TRANSLATOR_TARGET_LANGUAGE,
    enableVadEndpointing: true,
    vadModel: DEFAULT_VAD_MODEL,
    endpointMechanism: DEFAULT_ENDPOINT_MECHANISM,
    diarizationModel: DEFAULT_DIARIZATION_MODEL,
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

  const trustedStorageReady = chrome.storage.local.setAccessLevel({
    accessLevel: 'TRUSTED_CONTEXTS',
  });
  const settingsReady = trustedStorageReady
    .then(() => chrome.storage.local.get({
      provider: 'local',
      modelId: DEFAULT_MODEL_ID,
      quantId: DEFAULT_QUANT_ID,
      hiddenModelIds: [] as string[],
      customModels: [] as unknown[],
      customRepo: '',
      customFilename: '',
      geminiApiKey: '',
      sonioxApiKey: '',
      elevenlabsApiKey: '',
      googleModelId: '',
      googleModels: [] as unknown[],
      elevenLabsModelId: '',
      elevenLabsModels: [] as unknown[],
      googleTargetLanguage: DEFAULT_GOOGLE_TARGET_LANGUAGE,
      translatorEngine: DEFAULT_TRANSLATOR_ENGINE,
      translatorTargetLanguage: DEFAULT_TRANSLATOR_TARGET_LANGUAGE,
      enableVadEndpointing: true,
      vadModel: DEFAULT_VAD_MODEL,
      endpointMechanism: DEFAULT_ENDPOINT_MECHANISM,
      diarizationModel: '',
      language: DEFAULT_LANGUAGE,
      targetLanguage: DEFAULT_TARGET_LANGUAGE,
      showFurigana: true,
      enableDiarization: false,
      subtitleStyle: DEFAULT_SUBTITLE_STYLE,
      serverUrl: DEFAULT_SERVER_URL,
    }))
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
      const googleModels = sanitizeGoogleModels(saved.googleModels);
      const activeGoogle = pickDefaultGoogleModel(
        googleModels,
        typeof saved.googleModelId === 'string' ? saved.googleModelId : undefined,
      );

      state.provider = sanitizeProvider(saved.provider);
      state.customModels = migrated.customModels;
      state.hiddenModelIds = hidden;
      state.modelId = active.id;
      state.quantId = active.id === migrated.modelId ? migrated.quantId : active.defaultVariantId;
      state.geminiApiKey = typeof saved.geminiApiKey === 'string' ? saved.geminiApiKey.trim() : '';
      state.speechApiKeys = {
        soniox: typeof saved.sonioxApiKey === 'string' ? saved.sonioxApiKey.trim() : '',
        elevenlabs: typeof saved.elevenlabsApiKey === 'string' ? saved.elevenlabsApiKey.trim() : '',
      };
      state.googleModels = googleModels;
      state.googleModelId = activeGoogle?.id ?? '';
      state.elevenLabsModels = sanitizeElevenLabsModels(saved.elevenLabsModels);
      state.elevenLabsModelId = state.elevenLabsModels.find((model) => model.id === saved.elevenLabsModelId)?.id
        ?? state.elevenLabsModels[0]?.id ?? '';
      state.googleTargetLanguage = sanitizeGoogleTargetLanguage(saved.googleTargetLanguage);
      state.translatorEngine = sanitizeTranslatorEngine(saved.translatorEngine);
      state.translatorTargetLanguage = sanitizeTranslatorTargetLanguage(
        saved.translatorTargetLanguage,
      );
      state.enableVadEndpointing = saved.enableVadEndpointing !== false;
      state.vadModel = sanitizeVadModel(saved.vadModel);
      state.endpointMechanism = sanitizeEndpointMechanism(saved.endpointMechanism);
      state.diarizationModel = sanitizeDiarizationModel(
        saved.diarizationModel,
        Boolean(saved.enableDiarization),
      );
      state.language = String(saved.language || DEFAULT_LANGUAGE);
      state.targetLanguage = sanitizeTargetLanguage(saved.targetLanguage);
      state.showFurigana = saved.showFurigana !== false;
      state.enableDiarization = state.diarizationModel !== 'disabled';
      state.subtitleStyle = sanitizeSubtitleStyle(
        saved.subtitleStyle as Partial<SubtitleStyle> | undefined,
      );
      state.serverUrl = portToServerUrl(
        parsePortFromServerUrl(String(saved.serverUrl || DEFAULT_SERVER_URL)),
      );
    })
    .catch(() => {});
  let credentialWrites = settingsReady;
  let elevenLabsWrites = settingsReady;

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
      justification: 'Capture tab audio and stream 16 kHz PCM to the native subtitle server',
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
    const provider = sanitizeProvider(msg.provider ?? state.provider);
    state.provider = provider;

    if (provider !== 'local') {
      const apiKey = provider === 'google' ? state.geminiApiKey : state.speechApiKeys[provider];
      const label = CLOUD_PROVIDERS.find(({ id }) => id === provider)?.label ?? provider;
      if (!apiKey) {
        state.status = 'error';
        state.error = `Set your ${label} API key in Settings before starting.`;
        broadcastState();
        return;
      }
      const googleModel = provider === 'google'
        ? msg.googleModel ?? pickDefaultGoogleModel(state.googleModels, state.googleModelId)
        : undefined;
      if (provider === 'google' && !googleModel) {
        state.status = 'error';
        state.error = 'Load and select a Google model before starting.';
        broadcastState();
        return;
      }
      let elevenLabsModelId: string | undefined;
      if (provider === 'elevenlabs') {
        await elevenLabsWrites;
        elevenLabsModelId = msg.elevenLabsModelId || state.elevenLabsModelId || undefined;
        if (elevenLabsModelId && !state.elevenLabsModels.some((model) => model.id === elevenLabsModelId)) {
          state.status = 'error';
          state.error = 'Select an available ElevenLabs model before starting.';
          broadcastState();
          return;
        }
      }
      const serverUrl = portToServerUrl(
        parsePortFromServerUrl(msg.serverUrl || state.serverUrl || DEFAULT_SERVER_URL),
      );
      const diarizationModel = sanitizeDiarizationModel(
        msg.diarizationModel ?? state.diarizationModel,
        msg.enableDiarization ?? state.enableDiarization,
      );
      const enableDiarization = diarizationModel !== 'disabled';
      const enableVadEndpointing = msg.enableVadEndpointing ?? state.enableVadEndpointing;
      const vadModel = sanitizeVadModel(msg.vadModel ?? state.vadModel);
      const endpointMechanism = sanitizeEndpointMechanism(
        msg.endpointMechanism ?? state.endpointMechanism,
      );
      const supportsDirect = googleModel ? googleModelSupportsDirectS2tt(googleModel.kind) : false;
      const googleTargetLanguage = supportsDirect
        ? sanitizeGoogleTargetLanguage(msg.googleTargetLanguage ?? state.googleTargetLanguage)
        : 'same';
      const chosenTranslatorEngine = sanitizeTranslatorEngine(
        msg.translatorEngine ?? state.translatorEngine,
      );
      const effectiveTranslatorEngine = supportsDirect ? 'disabled' : chosenTranslatorEngine;
      const translatorTargetLanguage = sanitizeTranslatorTargetLanguage(
        msg.translatorTargetLanguage ?? state.translatorTargetLanguage,
      );

      state.language = msg.language;
      if (supportsDirect) {
        state.googleTargetLanguage = googleTargetLanguage;
      } else {
        state.translatorEngine = chosenTranslatorEngine;
      }
      state.translatorTargetLanguage = translatorTargetLanguage;
      if (googleModel) state.googleModelId = googleModel.id;
      state.enableVadEndpointing = enableVadEndpointing;
      state.vadModel = vadModel;
      state.endpointMechanism = endpointMechanism;
      state.diarizationModel = diarizationModel;
      state.enableDiarization = enableDiarization;
      state.serverUrl = serverUrl;
      await chrome.storage.local
        .set({
          provider,
          googleModelId: state.googleModelId,
          language: msg.language,
          googleTargetLanguage: state.googleTargetLanguage,
          translatorEngine: state.translatorEngine,
          translatorTargetLanguage,
          enableVadEndpointing,
          vadModel,
          endpointMechanism,
          diarizationModel,
          enableDiarization,
          serverUrl,
        })
        .catch(() => {});

      if (state.tabId !== null && state.tabId !== msg.tabId) {
        await sendSubtitleToTab(state.tabId, [], false);
      }

      state.status = 'loading';
      state.tabId = msg.tabId;
      state.device = provider;
      state.activity = '';
      state.error = null;
      state.lastText = '';
      subtitleLines = [];
      state.progress = null;
      state.progressLabel = `Connecting to ${label}...`;
      state.progressSpeedBps = null;
      broadcastState();

      try {
        await sendSubtitleToTab(msg.tabId, [], false);
        await ensureOffscreen();
        const streamId = await getTabStreamId(msg.tabId);
        const commonPayload = {
          type: 'OFFSCREEN_START' as const,
          streamId,
          apiKey,
          language: msg.language,
          translatorEngine: effectiveTranslatorEngine,
          translatorTargetLanguage,
          diarizationModel,
          enableDiarization,
          serverUrl,
        };
        if (provider !== 'google') {
          await chrome.runtime.sendMessage({
            ...commonPayload, provider, elevenLabsModelId,
          } satisfies BackgroundToOffscreenMessage);
        } else if (googleModel) {
          await chrome.runtime.sendMessage({
            ...commonPayload,
            provider,
            model: googleModel,
            targetLanguage: googleTargetLanguage,
            enableVadEndpointing,
            vadModel,
            endpointMechanism,
          } satisfies BackgroundToOffscreenMessage);
        }
      } catch (err) {
        state.status = 'error';
        state.error = err instanceof Error ? err.message : String(err);
        state.progress = null;
        state.progressLabel = '';
        state.progressSpeedBps = null;
        broadcastState();
      }
      return;
    }

    const serverUrl = portToServerUrl(
      parsePortFromServerUrl(msg.serverUrl || state.serverUrl || DEFAULT_SERVER_URL),
    );
    const diarizationModel = sanitizeDiarizationModel(
      msg.diarizationModel ?? state.diarizationModel,
      msg.enableDiarization ?? state.enableDiarization,
    );
    const enableDiarization = diarizationModel !== 'disabled';
    const enableVadEndpointing = msg.enableVadEndpointing ?? state.enableVadEndpointing;
    const vadModel = sanitizeVadModel(msg.vadModel ?? state.vadModel);
    const endpointMechanism = sanitizeEndpointMechanism(
      msg.endpointMechanism ?? state.endpointMechanism,
    );
    const targetLanguage = sanitizeTargetLanguage(msg.targetLanguage ?? state.targetLanguage);
    let source = msg.source;
    if (!source) {
      const model = findModelEntry(msg.modelId, state.customModels);
      try {
        const variants = model.isCustom ? model.variants : await loadModelVariants(model);
        source = resolveModelSource({ ...model, variants }, msg.quantId);
        if (!source) throw new Error(`No quantizations available for ${model.label}.`);
      } catch (err) {
        state.status = 'error';
        state.error = err instanceof Error ? err.message : String(err);
        broadcastState();
        return;
      }
    }
    const chosenTranslatorEngine = sanitizeTranslatorEngine(
      msg.translatorEngine ?? state.translatorEngine,
    );
    const effectiveTranslatorEngine = source.supportsDirectS2tt
      ? 'disabled'
      : chosenTranslatorEngine;
    const translatorTargetLanguage = sanitizeTranslatorTargetLanguage(
      msg.translatorTargetLanguage ?? state.translatorTargetLanguage,
    );

    state.modelId = msg.modelId;
    state.quantId = msg.quantId;
    state.language = msg.language;
    state.targetLanguage = targetLanguage;
    if (!source.supportsDirectS2tt) {
      state.translatorEngine = chosenTranslatorEngine;
    }
    state.translatorTargetLanguage = translatorTargetLanguage;
    state.enableVadEndpointing = enableVadEndpointing;
    state.vadModel = vadModel;
    state.endpointMechanism = endpointMechanism;
    state.diarizationModel = diarizationModel;
    state.enableDiarization = enableDiarization;
    state.serverUrl = serverUrl;
    await chrome.storage.local
      .set({
        provider: 'local',
        modelId: msg.modelId,
        quantId: msg.quantId,
        language: msg.language,
        targetLanguage,
        translatorEngine: state.translatorEngine,
        translatorTargetLanguage,
        enableVadEndpointing,
        vadModel,
        endpointMechanism,
        diarizationModel,
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
      const translate =
        Boolean(source.supportsDirectS2tt) &&
        targetLanguage === 'en' &&
        msg.language !== 'en';

      await chrome.runtime.sendMessage({
        type: 'OFFSCREEN_START',
        provider: 'local',
        streamId,
        source,
        language: msg.language,
        translate,
        translatorEngine: effectiveTranslatorEngine,
        translatorTargetLanguage,
        enableVadEndpointing,
        vadModel,
        endpointMechanism,
        diarizationModel,
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

  function startServerDownload(
    filename: string,
    displayLabel: string,
    payload: Record<string, unknown>,
    serverUrlInput?: string,
  ): void {
    if (state.downloadingFilename !== null || downloadWs !== null) return;

    const serverUrl = portToServerUrl(
      parsePortFromServerUrl(serverUrlInput || state.serverUrl || DEFAULT_SERVER_URL),
    );
    state.downloadingFilename = filename;
    state.downloadProgress = 0;
    state.downloadLabel = `Connecting to download ${displayLabel}...`;
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
        ws?.send(JSON.stringify(payload));
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
            state.downloadLabel = data.label ?? `Downloading ${displayLabel}...`;
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

  function startModelDownload(
    msg: Extract<PopupToBackgroundMessage, { type: 'DOWNLOAD_MODEL' }>,
  ): void {
    const filename = msg.source.filename?.trim();
    if (!filename) {
      state.error = 'Select a model before downloading.';
      broadcastState();
      return;
    }
    startServerDownload(
      filename,
      filename,
      {
        type: 'downloadModel',
        filename,
        repo: msg.source.repo,
        url: msg.source.url,
      },
      msg.serverUrl,
    );
  }

  function startTranslatorDownload(
    msg: Extract<PopupToBackgroundMessage, { type: 'DOWNLOAD_TRANSLATOR' }>,
  ): void {
    startServerDownload(
      'hy-mt3-1.8b.gguf',
      'Hy-MT3-1.8B',
      { type: 'downloadTranslator' },
      msg.serverUrl,
    );
  }

  chrome.runtime.onMessage.addListener(
    (
      message: PopupToBackgroundMessage | OffscreenToBackgroundMessage,
      sender,
      sendResponse,
    ) => {
      if (!sender.url?.startsWith(chrome.runtime.getURL('/'))) return false;

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

      if (message.type === 'DOWNLOAD_TRANSLATOR') {
        startTranslatorDownload(message);
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

      if (message.type === 'SET_PROVIDER') {
        state.provider = sanitizeProvider(message.provider);
        void chrome.storage.local.set({ provider: state.provider }).catch(() => {});
        broadcastState();
        return false;
      }

      if (message.type === 'SET_GEMINI_API_KEY' || message.type === 'SET_SPEECH_API_KEY') {
        const isGemini = message.type === 'SET_GEMINI_API_KEY';
        const provider = isGemini ? 'google' : sanitizeProvider(message.provider);
        if (provider === 'local' || (!isGemini && provider === 'google') || typeof message.apiKey !== 'string') {
          sendResponse({ error: 'Invalid provider or API key.' });
          return false;
        }
        const apiKey = message.apiKey.trim();
        const saved = credentialWrites.then(async () => {
          await trustedStorageReady;
          if (isGemini) {
            const googleModels = apiKey ? sanitizeGoogleModels(message.models ?? state.googleModels) : [];
            const googleModelId = pickDefaultGoogleModel(
              googleModels, message.googleModelId ?? state.googleModelId,
            )?.id ?? '';
            const settings = { geminiApiKey: apiKey, googleModels, googleModelId };
            await chrome.storage.local.set(settings);
            Object.assign(state, settings);
          } else if (provider !== 'google') {
            await chrome.storage.local.set({ [`${provider}ApiKey`]: apiKey });
            state.speechApiKeys[provider] = apiKey;
          }
          broadcastState();
        });
        credentialWrites = saved.catch(() => {});
        saved.then(
          () => sendResponse({ ok: true }),
          () => sendResponse({ error: 'Could not update the saved API key.' }),
        );
        return true;
      }

      if (message.type === 'SET_GOOGLE_MODELS') {
        state.googleModels = sanitizeGoogleModels(message.models);
        state.googleModelId =
          pickDefaultGoogleModel(
            state.googleModels,
            message.googleModelId ?? state.googleModelId,
          )?.id ?? '';
        void chrome.storage.local
          .set({
            googleModels: state.googleModels,
            googleModelId: state.googleModelId,
          })
          .catch(() => {});
        broadcastState();
        return false;
      }

      if (message.type === 'SET_GOOGLE_MODEL') {
        const chosen = pickDefaultGoogleModel(state.googleModels, message.googleModelId);
        state.googleModelId = chosen?.id ?? message.googleModelId;
        void chrome.storage.local.set({ googleModelId: state.googleModelId }).catch(() => {});
        broadcastState();
        return false;
      }

      if (message.type === 'REFRESH_ELEVENLABS_MODELS' || message.type === 'SET_ELEVENLABS_MODEL') {
        const updateModels = async () => {
          await settingsReady;
          const fetched = message.type === 'REFRESH_ELEVENLABS_MODELS'
            ? await fetchElevenLabsModelsViaServer(state.serverUrl)
            : undefined;
          const saved = elevenLabsWrites.then(async () => {
            const models = fetched ?? state.elevenLabsModels;
            const preferredId = message.type === 'SET_ELEVENLABS_MODEL'
              ? message.modelId
              : state.elevenLabsModelId;
            const selected = models.find((model) => model.id === preferredId);
            if (!selected && message.type === 'SET_ELEVENLABS_MODEL') {
              throw new Error('That ElevenLabs model is no longer available.');
            }
            const settings = {
              elevenLabsModels: models,
              elevenLabsModelId: selected?.id ?? models[0]?.id ?? '',
            };
            await chrome.storage.local.set(settings);
            Object.assign(state, settings);
            broadcastState();
          });
          elevenLabsWrites = saved.catch(() => {});
          await saved;
        };
        updateModels().then(
          () => sendResponse({ ok: true }),
          (err) => sendResponse({ error: err instanceof Error ? err.message : 'Could not update ElevenLabs models.' }),
        );
        return true;
      }

      if (message.type === 'SET_LANGUAGE') {
        state.language = message.language || DEFAULT_LANGUAGE;
        void chrome.storage.local.set({ language: state.language }).catch(() => {});
        broadcastState();
        return false;
      }

      if (message.type === 'SET_TARGET_LANGUAGE') {
        state.targetLanguage = sanitizeTargetLanguage(message.targetLanguage);
        void chrome.storage.local.set({ targetLanguage: state.targetLanguage }).catch(() => {});
        broadcastState();
        return false;
      }

      if (message.type === 'SET_GOOGLE_TARGET_LANGUAGE') {
        state.googleTargetLanguage = sanitizeGoogleTargetLanguage(message.targetLanguage);
        void chrome.storage.local
          .set({ googleTargetLanguage: state.googleTargetLanguage })
          .catch(() => {});
        broadcastState();
        return false;
      }

      if (message.type === 'SET_TRANSLATOR_ENGINE') {
        state.translatorEngine = sanitizeTranslatorEngine(message.translatorEngine);
        void chrome.storage.local
          .set({ translatorEngine: state.translatorEngine })
          .catch(() => {});
        broadcastState();
        return false;
      }

      if (message.type === 'SET_TRANSLATOR_TARGET_LANGUAGE') {
        state.translatorTargetLanguage = sanitizeTranslatorTargetLanguage(message.targetLanguage);
        void chrome.storage.local
          .set({ translatorTargetLanguage: state.translatorTargetLanguage })
          .catch(() => {});
        broadcastState();
        return false;
      }

      if (message.type === 'SET_VAD_ENDPOINTING') {
        state.enableVadEndpointing = Boolean(message.enabled);
        void chrome.storage.local
          .set({ enableVadEndpointing: state.enableVadEndpointing })
          .catch(() => {});
        broadcastState();
        return false;
      }

      if (message.type === 'SET_VAD_MODEL') {
        state.vadModel = sanitizeVadModel(message.vadModel);
        void chrome.storage.local.set({ vadModel: state.vadModel }).catch(() => {});
        broadcastState();
        return false;
      }

      if (message.type === 'SET_ENDPOINT_MECHANISM') {
        state.endpointMechanism = sanitizeEndpointMechanism(message.endpointMechanism);
        void chrome.storage.local
          .set({ endpointMechanism: state.endpointMechanism })
          .catch(() => {});
        broadcastState();
        return false;
      }

      if (message.type === 'SET_DIARIZATION_MODEL') {
        state.diarizationModel = sanitizeDiarizationModel(message.diarizationModel, false);
        state.enableDiarization = state.diarizationModel !== 'disabled';
        void chrome.storage.local
          .set({
            diarizationModel: state.diarizationModel,
            enableDiarization: state.enableDiarization,
          })
          .catch(() => {});
        if (state.status === 'listening' || state.status === 'loading') {
          void chrome.runtime
            .sendMessage({
              type: 'OFFSCREEN_SET_DIARIZATION',
              enabled: state.enableDiarization,
              model: state.diarizationModel,
            })
            .catch(() => {});
        }
        broadcastState();
        return false;
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
        if (active.id !== state.modelId) {
          state.modelId = active.id;
          state.quantId = active.defaultVariantId;
        }

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
