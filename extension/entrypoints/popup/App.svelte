<script lang="ts">
  import { onMount } from 'svelte';
  import CollapsibleCard from './CollapsibleCard.svelte';
  import {
    DEFAULT_SERVER_PORT,
    DEFAULT_SUBTITLE_STYLE,
    hexToRgba,
    parsePortFromServerUrl,
    portToServerUrl,
    sanitizePort,
    sanitizeSubtitleStyle,
    type BackgroundBroadcastMessage,
    type ExtensionState,
    type SubtitleStyle,
  } from '../../lib/messages';
  import {
    BUILTIN_MODELS,
    DEFAULT_LANGUAGE,
    DEFAULT_MODEL_ID,
    DEFAULT_QUANT_ID,
    DEFAULT_SERVER_URL,
    DEFAULT_TARGET_LANGUAGE,
    LANGUAGES,
    TARGET_LANGUAGES,
    buildFilenameLabelMap,
    buildVisibleModels,
    formatBytes,
    formatSpeedMBps,
    isValidCustomFilename,
    pickVariantForModel,
    resolveModelSource,
    sanitizeTargetLanguage,
    type CustomModelDefinition,
    type TargetLanguage,
  } from '../../lib/models';

  type ServerHealth = 'checking' | 'online' | 'offline';
  type LocalFileInfo = { filename: string; bytes: number; supportsDirectS2tt?: boolean };
  type PopupTab = 'model' | 'display' | 'settings';

  const appVersion = `v${chrome.runtime.getManifest().version || '0.1.0'}`;

  let statusState = $state<ExtensionState>({
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
  });

  let activeTab = $state<PopupTab>('model');
  let selectedModelId = $state(DEFAULT_MODEL_ID);
  let selectedQuantId = $state(DEFAULT_QUANT_ID);
  let hiddenModelIds = $state<string[]>([]);
  let customModels = $state<CustomModelDefinition[]>([]);
  let selectedLanguage = $state(DEFAULT_LANGUAGE);
  let targetLanguage = $state<TargetLanguage>(DEFAULT_TARGET_LANGUAGE);
  let showFurigana = $state(true);
  let enableDiarization = $state(false);
  let subtitleStyle = $state<SubtitleStyle>({ ...DEFAULT_SUBTITLE_STYLE });
  let portInput = $state(String(DEFAULT_SERVER_PORT));
  let portError = $state<string | null>(null);
  let portSavedMsg = $state('');

  let newCustomLabel = $state('');
  let newCustomDesc = $state('');
  let newCustomRepo = $state('');
  let newCustomFilename = $state('');
  let catalogError = $state<string | null>(null);

  let localError = $state<string | null>(null);
  let deletingFilename = $state<string | null>(null);
  const deleting = $derived(deletingFilename !== null);
  let openSections = $state({
    features: true,
    subtitleStyle: true,
    serverConnection: true,
    modelList: true,
    downloadedModels: true,
  });
  let serverHealth = $state<ServerHealth>('checking');
  let nativeFiles = $state<Record<string, LocalFileInfo>>({});
  let nativeCacheDir = $state('');

  let modelMenuOpen = $state(false);
  let dropdownRef = $state<HTMLDivElement | null>(null);

  const activePort = $derived(parsePortFromServerUrl(statusState.serverUrl || DEFAULT_SERVER_URL));
  const activeServerUrl = $derived(portToServerUrl(activePort));
  const visibleModels = $derived(buildVisibleModels(hiddenModelIds, customModels));
  const selectedModel = $derived(
    visibleModels.find((m) => m.id === selectedModelId) ?? visibleModels[0],
  );
  const activeVariant = $derived(pickVariantForModel(selectedModel, selectedQuantId));
  const selectedVariantIndex = $derived(
    Math.max(
      0,
      selectedModel.variants.findIndex((v) => v.id === activeVariant.id),
    ),
  );
  const modelSource = $derived(
    resolveModelSource(selectedModel.id, activeVariant.id, customModels),
  );
  const localFileInfo = $derived(
    modelSource.filename ? nativeFiles[modelSource.filename] : undefined,
  );
  const downloadedSize = $derived(localFileInfo?.bytes);
  const localFileList = $derived<LocalFileInfo[]>(
    Object.values(nativeFiles).sort((a, b) => a.filename.localeCompare(b.filename)),
  );
  const filenameLabelMap = $derived(buildFilenameLabelMap(customModels));
  const unlistedLocalFiles = $derived(
    localFileList.filter((file) => !filenameLabelMap.has(file.filename)),
  );
  const totalDownloadedBytes = $derived(
    localFileList.reduce((sum, file) => sum + file.bytes, 0),
  );
  const downloadedModelsMeta = $derived(
    serverHealth !== 'online'
      ? ''
      : localFileList.length === 0
        ? '0'
        : `${localFileList.length} · ${formatBytes(totalDownloadedBytes)}`,
  );
  const isJapaneseOnly = $derived(Boolean(modelSource.japaneseOnly));
  const supportsDirectS2tt = $derived.by(() => {
    if (
      selectedModel.isCustom &&
      localFileInfo &&
      typeof localFileInfo.supportsDirectS2tt === 'boolean'
    ) {
      return localFileInfo.supportsDirectS2tt;
    }
    return Boolean(modelSource.supportsDirectS2tt);
  });
  const isDownloading = $derived(statusState.downloadingFilename !== null);
  const isCapturingState = $derived(
    statusState.status === 'loading' || statusState.status === 'listening',
  );
  const isBusy = $derived(isCapturingState || isDownloading);
  const startBlocked = $derived(
    !isCapturingState && (serverHealth !== 'online' || !modelSource.filename || !downloadedSize),
  );
  const displayError = $derived(localError ?? statusState.error);

  const statusBadge = $derived.by(() => {
    if (statusState.status === 'listening') {
      return statusState.device === 'metal' ? 'Listening · METAL' : 'Listening · VULKAN';
    }
    if (statusState.status === 'loading' || isDownloading) return 'Downloading';
    if (statusState.status === 'error') return 'Error';
    return 'Idle';
  });

  const activeProgress = $derived(
    isDownloading
      ? statusState.downloadProgress
      : statusState.status === 'loading'
        ? statusState.progress
        : null,
  );
  const activeProgressLabel = $derived(
    isDownloading
      ? statusState.downloadLabel
      : statusState.status === 'loading'
        ? statusState.progressLabel
        : '',
  );
  const activeSpeedBps = $derived(
    isDownloading
      ? statusState.downloadSpeedBps
      : statusState.status === 'loading'
        ? statusState.progressSpeedBps
        : null,
  );
  const activeSpeedLabel = $derived(
    activeSpeedBps !== null && activeSpeedBps > 0 ? formatSpeedMBps(activeSpeedBps) : '',
  );
  const showProgress = $derived(isDownloading || statusState.status === 'loading');

  function applyModelsPayload(data: { cacheDir?: string; files?: LocalFileInfo[] }) {
    if (data.cacheDir) nativeCacheDir = data.cacheDir;
    if (Array.isArray(data.files)) {
      const next: Record<string, LocalFileInfo> = {};
      for (const file of data.files) {
        next[file.filename] = file;
      }
      nativeFiles = next;
    }
  }

  function pingServer(url = activeServerUrl, silent = false) {
    if (!silent) {
      serverHealth = 'checking';
    }
    const markOffline = () => {
      serverHealth = 'offline';
      nativeFiles = {};
      nativeCacheDir = '';
    };
    try {
      const ws = new WebSocket(url);
      let gotPong = false;
      let gotModels = false;
      const timer = setTimeout(() => {
        ws.close();
        if (!gotPong) markOffline();
      }, 1500);

      ws.onopen = () => {
        ws.send(JSON.stringify({ type: 'ping' }));
        ws.send(JSON.stringify({ type: 'listModels' }));
      };
      ws.onmessage = (ev) => {
        try {
          const data = JSON.parse(String(ev.data)) as {
            type?: string;
            cacheDir?: string;
            files?: LocalFileInfo[];
          };
          if (data.type === 'pong') {
            gotPong = true;
            serverHealth = 'online';
          } else if (data.type === 'models') {
            gotModels = true;
            applyModelsPayload(data);
          }
          if (gotPong && gotModels) {
            clearTimeout(timer);
            ws.close();
          }
        } catch {
          // Ignore malformed messages
        }
      };
      ws.onerror = () => {
        clearTimeout(timer);
        markOffline();
      };
    } catch {
      markOffline();
    }
  }

  function switchTab(tab: PopupTab) {
    activeTab = tab;
    modelMenuOpen = false;
    portSavedMsg = '';
  }

  function applyPort(rawPort: string | number) {
    const validPort = sanitizePort(rawPort);
    if (validPort === null) {
      portError = 'Enter a valid port between 1 and 65535.';
      portSavedMsg = '';
      return;
    }

    portError = null;
    const nextUrl = portToServerUrl(validPort);
    portInput = String(validPort);
    statusState = { ...statusState, serverUrl: nextUrl };
    portSavedMsg = 'Saved';
    chrome.runtime.sendMessage({ type: 'SET_SERVER_URL', serverUrl: nextUrl }, () => {
      void chrome.runtime.lastError;
    });
    pingServer(nextUrl);
  }

  function resetPort() {
    applyPort(DEFAULT_SERVER_PORT);
  }

  function toggleMenu() {
    if (isBusy || deleting) return;
    modelMenuOpen = !modelMenuOpen;
  }

  function selectModel(id: string) {
    selectedModelId = id;
    const nextModel = visibleModels.find((m) => m.id === id) ?? visibleModels[0];
    selectedQuantId = pickVariantForModel(nextModel, selectedQuantId).id;
    modelMenuOpen = false;
    localError = null;
  }

  function saveModelCatalog(
    nextHidden: string[],
    nextCustom: CustomModelDefinition[],
  ) {
    hiddenModelIds = nextHidden;
    customModels = nextCustom;
    const nextVisible = buildVisibleModels(nextHidden, nextCustom);
    const active = nextVisible.find((m) => m.id === selectedModelId) ?? nextVisible[0];
    selectedModelId = active.id;
    selectedQuantId = pickVariantForModel(active, selectedQuantId).id;

    chrome.runtime.sendMessage(
      {
        type: 'SET_MODEL_CATALOG',
        hiddenModelIds: nextHidden,
        customModels: nextCustom,
      },
      () => {
        void chrome.runtime.lastError;
      },
    );
  }

  function toggleBuiltinVisibility(id: string) {
    const isHidden = hiddenModelIds.includes(id);
    if (!isHidden && visibleModels.length <= 1) return;
    const nextHidden = isHidden
      ? hiddenModelIds.filter((item) => item !== id)
      : [...hiddenModelIds, id];
    catalogError = null;
    saveModelCatalog(nextHidden, customModels);
  }

  function handleAddCustomModel() {
    const filename = newCustomFilename.trim();
    if (!isValidCustomFilename(filename)) {
      catalogError = 'Enter a valid .bin filename (for example ggml-large-v3-q8_0.bin).';
      return;
    }
    if (customModels.some((m) => m.filename === filename)) {
      catalogError = `"${filename}" is already in your custom models.`;
      return;
    }
    const repo = newCustomRepo.trim();
    const label = newCustomLabel.trim() || filename.replace(/\.bin$/i, '');
    const description = newCustomDesc.trim() || 'Custom GGML model.';
    const created: CustomModelDefinition = {
      id: `custom:${filename}`,
      label,
      description,
      ...(repo ? { repo } : {}),
      filename,
    };

    catalogError = null;
    const nextCustom = [...customModels, created];
    saveModelCatalog(hiddenModelIds, nextCustom);
    selectedModelId = created.id;
    selectedQuantId = 'custom';
    newCustomLabel = '';
    newCustomDesc = '';
    newCustomRepo = '';
    newCustomFilename = '';
  }

  function handleRemoveCustomModel(id: string) {
    catalogError = null;
    saveModelCatalog(
      hiddenModelIds,
      customModels.filter((m) => m.id !== id),
    );
  }

  function handleWindowClick(e: MouseEvent) {
    if (!modelMenuOpen || !dropdownRef) return;
    if (!dropdownRef.contains(e.target as Node)) {
      modelMenuOpen = false;
    }
  }

  function handleWindowKeydown(e: KeyboardEvent) {
    if (e.key === 'Escape' && modelMenuOpen) {
      modelMenuOpen = false;
    }
  }

  onMount(() => {
    chrome.runtime.sendMessage({ type: 'GET_STATE' }, (res: ExtensionState | undefined) => {
      if (chrome.runtime.lastError || !res) {
        pingServer();
        return;
      }
      statusState = res;
      hiddenModelIds = res.hiddenModelIds ?? [];
      customModels = res.customModels ?? [];
      selectedModelId = res.modelId || DEFAULT_MODEL_ID;
      selectedQuantId = res.quantId || DEFAULT_QUANT_ID;
      selectedLanguage = res.language || DEFAULT_LANGUAGE;
      targetLanguage = sanitizeTargetLanguage(res.targetLanguage);
      showFurigana = res.showFurigana !== false;
      enableDiarization = Boolean(res.enableDiarization);
      subtitleStyle = sanitizeSubtitleStyle(res.subtitleStyle);
      const url = res.serverUrl || DEFAULT_SERVER_URL;
      portInput = String(parsePortFromServerUrl(url));
      pingServer(url);
    });

    const onMessage = (msg: BackgroundBroadcastMessage) => {
      if (msg.type === 'STATE_CHANGED') {
        const prevStatus = statusState.status;
        const prevDownloading = statusState.downloadingFilename;
        statusState = msg.state;
        hiddenModelIds = msg.state.hiddenModelIds ?? [];
        customModels = msg.state.customModels ?? [];
        if (msg.state.modelId) selectedModelId = msg.state.modelId;
        if (msg.state.quantId) selectedQuantId = msg.state.quantId;
        targetLanguage = sanitizeTargetLanguage(msg.state.targetLanguage);
        showFurigana = msg.state.showFurigana !== false;
        enableDiarization = Boolean(msg.state.enableDiarization);
        if (msg.state.subtitleStyle) {
          subtitleStyle = sanitizeSubtitleStyle(msg.state.subtitleStyle);
        }
        if (!portError && msg.state.serverUrl) {
          portInput = String(parsePortFromServerUrl(msg.state.serverUrl));
        }
        if (
          (prevStatus !== 'listening' && msg.state.status === 'listening') ||
          (prevDownloading !== null && msg.state.downloadingFilename === null)
        ) {
          pingServer(msg.state.serverUrl || activeServerUrl, true);
        }
      }
    };
    chrome.runtime.onMessage.addListener(onMessage);
    return () => chrome.runtime.onMessage.removeListener(onMessage);
  });

  function handleDownload() {
    if (isBusy || deleting) return;
    const src = modelSource;
    if (!src.filename) {
      localError = 'Enter a .bin filename first.';
      return;
    }
    localError = null;
    chrome.runtime.sendMessage(
      {
        type: 'DOWNLOAD_MODEL',
        source: src,
        serverUrl: activeServerUrl,
      },
      (res: ExtensionState | undefined) => {
        if (chrome.runtime.lastError) {
          localError = chrome.runtime.lastError.message ?? 'Failed to start download';
          return;
        }
        if (res) statusState = res;
      },
    );
  }

  function handleCancelDownload() {
    localError = null;
    chrome.runtime.sendMessage({ type: 'CANCEL_DOWNLOAD' }, (res: ExtensionState | undefined) => {
      if (chrome.runtime.lastError) {
        localError = chrome.runtime.lastError.message ?? 'Failed to cancel download';
        return;
      }
      if (res) statusState = res;
    });
  }

  function handleDelete(filename = modelSource.filename) {
    if (isBusy || deleting || !filename) return;
    localError = null;
    deletingFilename = filename;

    const ws = new WebSocket(activeServerUrl);
    ws.onopen = () => {
      ws.send(JSON.stringify({ type: 'deleteModel', filename }));
    };
    ws.onmessage = (ev) => {
      try {
        const data = JSON.parse(String(ev.data)) as {
          type?: string;
          error?: string;
          cacheDir?: string;
          files?: LocalFileInfo[];
        };
        if (data.type === 'models') {
          applyModelsPayload(data);
          deletingFilename = null;
          ws.close();
        } else if (data.type === 'error') {
          localError = data.error ?? 'Delete failed';
          deletingFilename = null;
          ws.close();
        }
      } catch {
        // Ignore parse failures
      }
    };
    ws.onerror = () => {
      localError = 'Server offline — could not delete local model.';
      deletingFilename = null;
    };
  }

  async function handleStart() {
    localError = null;
    if (serverHealth !== 'online') {
      localError = `Native server is offline on port ${activePort}. Start it first before capturing.`;
      return;
    }
    if (!modelSource.filename) {
      localError = 'Enter or select a GGML .bin filename before starting.';
      return;
    }
    if (!downloadedSize) {
      localError =
        'This model is not downloaded yet. Click Download first before starting subtitles.';
      return;
    }

    const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
    if (!tab?.id || (tab.url && /^(chrome|chrome-extension|edge|about):/i.test(tab.url))) {
      localError = 'Open a standard webpage (e.g. YouTube or Twitch) before starting.';
      return;
    }

    const effectiveLanguage = isJapaneseOnly ? 'ja' : selectedLanguage;
    const effectiveTarget: TargetLanguage = supportsDirectS2tt ? targetLanguage : 'same';

    chrome.runtime.sendMessage(
      {
        type: 'START_CAPTURE',
        tabId: tab.id,
        modelId: selectedModel.id,
        quantId: activeVariant.id,
        source: { ...modelSource, supportsDirectS2tt },
        language: effectiveLanguage,
        targetLanguage: effectiveTarget,
        enableDiarization,
        serverUrl: activeServerUrl,
      },
      (res: ExtensionState | undefined) => {
        if (chrome.runtime.lastError) {
          localError = chrome.runtime.lastError.message ?? 'Failed to start capture';
          return;
        }
        if (res) statusState = res;
      },
    );
  }

  function handleToggleFurigana(enabled: boolean) {
    showFurigana = enabled;
    chrome.runtime.sendMessage({ type: 'SET_FURIGANA', enabled }, () => {
      void chrome.runtime.lastError;
    });
  }

  function handleToggleDiarization(enabled: boolean) {
    enableDiarization = enabled;
    chrome.runtime.sendMessage({ type: 'SET_DIARIZATION', enabled }, () => {
      void chrome.runtime.lastError;
    });
  }

  function updateStyle(patch: Partial<SubtitleStyle>) {
    subtitleStyle = sanitizeSubtitleStyle({ ...subtitleStyle, ...patch });
    chrome.runtime.sendMessage({ type: 'SET_STYLE', style: subtitleStyle }, () => {
      void chrome.runtime.lastError;
    });
  }

  function resetStyle() {
    updateStyle(DEFAULT_SUBTITLE_STYLE);
  }

  function handleStop() {
    localError = null;
    chrome.runtime.sendMessage({ type: 'STOP_CAPTURE' }, (res: ExtensionState | undefined) => {
      if (res) statusState = res;
    });
  }
</script>

<svelte:window onclick={handleWindowClick} onkeydown={handleWindowKeydown} />

<main class="panel">
  <header class="header">
    <div class="title-wrap">
      <h1>Live Subtitles</h1>
      <span class="version-tag">{appVersion}</span>
    </div>
    <span class="badge" data-status={isDownloading ? 'loading' : statusState.status}>
      {statusBadge}
    </span>
  </header>

  <div class="tab-bar" role="tablist" aria-label="Popup sections">
    <button
      type="button"
      role="tab"
      aria-selected={activeTab === 'model'}
      class="tab-btn"
      data-active={activeTab === 'model'}
      onclick={() => switchTab('model')}
    >
      Model
    </button>
    <button
      type="button"
      role="tab"
      aria-selected={activeTab === 'display'}
      class="tab-btn"
      data-active={activeTab === 'display'}
      onclick={() => switchTab('display')}
    >
      Display
    </button>
    <button
      type="button"
      role="tab"
      aria-selected={activeTab === 'settings'}
      class="tab-btn"
      data-active={activeTab === 'settings'}
      onclick={() => switchTab('settings')}
    >
      Settings
    </button>
  </div>

  {#if activeTab === 'model'}
    <div class="server-row">
      <span class="server-pill" data-health={serverHealth}>
        <span class="server-dot" aria-hidden="true"></span>
        {#if serverHealth === 'online'}
          Online
        {:else if serverHealth === 'checking'}
          Checking...
        {:else}
          Offline
        {/if}
      </span>
      <button class="secondary-btn" onclick={() => pingServer()} disabled={isBusy}>
        Refresh
      </button>
    </div>

    <div class="field" bind:this={dropdownRef}>
      <span>Model</span>
      <div class="dropdown">
        <button
          type="button"
          class="dropdown-trigger"
          aria-expanded={modelMenuOpen}
          disabled={isBusy || deleting}
          onclick={toggleMenu}
        >
          <span class="trigger-label">{selectedModel.label}</span>
          <span class="trigger-meta">
            {#if downloadedSize}
              <span class="pill-ok">Downloaded</span>
            {:else}
              <span class="pill-size">{activeVariant.sizeLabel}</span>
            {/if}
            <span class="caret">{modelMenuOpen ? '▲' : '▼'}</span>
          </span>
        </button>

        {#if modelMenuOpen}
          <div class="dropdown-menu" role="listbox">
            <div class="option-list">
              {#each visibleModels as model (model.id)}
                {@const anySaved = model.variants.some((v) => Boolean(nativeFiles[v.filename]))}
                {@const defaultOpt = pickVariantForModel(model)}
                <button
                  type="button"
                  role="option"
                  aria-selected={selectedModel.id === model.id}
                  class="option-row"
                  data-selected={selectedModel.id === model.id}
                  onclick={() => selectModel(model.id)}
                >
                  <div class="opt-main">
                    <span class="opt-name">{model.label}</span>
                    <span class="opt-right">
                      {#if anySaved}
                        <span class="pill-ok">Downloaded</span>
                      {:else if model.isCustom}
                        <span class="pill-size">Custom</span>
                      {:else}
                        <span class="pill-size">{defaultOpt.sizeLabel}</span>
                      {/if}
                    </span>
                  </div>
                  <div class="opt-desc">{model.description}</div>
                </button>
              {/each}
            </div>
          </div>
        {/if}
      </div>
    </div>

    {#if selectedModel.variants.length > 1}
      {@const fastestOpt = selectedModel.variants[0]}
      {@const bestOpt = selectedModel.variants[selectedModel.variants.length - 1]}
      <div class="quant-field">
        <div class="quant-head">
          <span>Quantization</span>
          <span class="style-val">
            {activeVariant.label} · {downloadedSize ? formatBytes(downloadedSize) : activeVariant.sizeLabel}
          </span>
        </div>
        <input
          type="range"
          min="0"
          max={selectedModel.variants.length - 1}
          step="1"
          value={selectedVariantIndex}
          disabled={isBusy || deleting}
          oninput={(e) => {
            const idx = Number(e.currentTarget.value);
            const chosen = selectedModel.variants[idx];
            if (chosen) selectedQuantId = chosen.id;
          }}
        />
        <div class="quant-scale">
          <span>Fastest ({fastestOpt.label})</span>
          <span>Best ({bestOpt.label})</span>
        </div>
      </div>
    {/if}

    <div class="cache-row">
      {#if serverHealth !== 'online'}
        <span class="hint">Start native server to download or manage models</span>
      {:else if downloadedSize}
        <span class="hint">On disk · {formatBytes(downloadedSize)}</span>
        <button
          class="secondary-btn danger"
          disabled={isBusy || deleting}
          onclick={() => handleDelete()}
        >
          {deletingFilename === modelSource.filename ? 'Deleting...' : 'Delete'}
        </button>
      {:else}
        <span class="hint">
          Not downloaded{!selectedModel.isCustom ? ` (${activeVariant.sizeLabel})` : ''}
        </span>
        <button
          class="secondary-btn"
          disabled={isBusy || deleting || !modelSource.filename}
          onclick={handleDownload}
        >
          Download
        </button>
      {/if}
    </div>

    <label class="field">
      <span>Spoken Language</span>
      <select
        value={isJapaneseOnly ? 'ja' : selectedLanguage}
        onchange={(e) => (selectedLanguage = e.currentTarget.value)}
        disabled={isBusy || isJapaneseOnly}
      >
        {#each LANGUAGES as lang (lang.code)}
          <option value={lang.code}>{lang.label}</option>
        {/each}
      </select>
      {#if isJapaneseOnly}
        <small class="hint">Japanese-only model</small>
      {/if}
    </label>

    {#if supportsDirectS2tt}
      <label class="field">
        <span>Target Language</span>
        <select
          value={targetLanguage}
          onchange={(e) => (targetLanguage = sanitizeTargetLanguage(e.currentTarget.value))}
          disabled={isBusy}
        >
          {#each TARGET_LANGUAGES as target (target.code)}
            <option value={target.code}>{target.label}</option>
          {/each}
        </select>
      </label>
    {/if}
  {:else if activeTab === 'display'}
    <CollapsibleCard title="Features" bind:open={openSections.features}>
      <label class="toggle-row">
        <span>Show Furigana (振り仮名)</span>
        <input
          type="checkbox"
          checked={showFurigana}
          disabled={deleting || isDownloading}
          onchange={(e) => handleToggleFurigana(e.currentTarget.checked)}
        />
      </label>

      <label class="toggle-row">
        <span>Speaker Labels (Diarization)</span>
        <input
          type="checkbox"
          checked={enableDiarization}
          disabled={deleting || isDownloading}
          onchange={(e) => handleToggleDiarization(e.currentTarget.checked)}
        />
      </label>
    </CollapsibleCard>

    <CollapsibleCard title="Subtitle Style" bind:open={openSections.subtitleStyle}>
      <div class="style-panel">
        <div class="style-row">
          <span>Font Size</span>
          <span class="style-val">{subtitleStyle.fontSize}px</span>
        </div>
        <input
          type="range"
          min="14"
          max="56"
          step="1"
          value={subtitleStyle.fontSize}
          oninput={(e) => updateStyle({ fontSize: Number(e.currentTarget.value) })}
        />

        <div class="color-grid">
          <label class="color-field">
            <span>Text Color</span>
            <input
              type="color"
              value={subtitleStyle.textColor}
              oninput={(e) => updateStyle({ textColor: e.currentTarget.value })}
            />
          </label>
          <label class="color-field">
            <span>Background</span>
            <input
              type="color"
              value={subtitleStyle.bgColor}
              oninput={(e) => updateStyle({ bgColor: e.currentTarget.value })}
            />
          </label>
        </div>

        <div class="style-row">
          <span>Background Opacity</span>
          <span class="style-val">{subtitleStyle.bgOpacity}%</span>
        </div>
        <input
          type="range"
          min="0"
          max="100"
          step="5"
          value={subtitleStyle.bgOpacity}
          oninput={(e) => updateStyle({ bgOpacity: Number(e.currentTarget.value) })}
        />

        <div class="style-preview-wrap">
          <span
            class="style-sample"
            style={`font-size:${Math.min(28, subtitleStyle.fontSize)}px;color:${subtitleStyle.textColor};background:${hexToRgba(subtitleStyle.bgColor, subtitleStyle.bgOpacity)};`}
          >
            今日 楽しい！
          </span>
          <button type="button" class="secondary-btn" onclick={resetStyle}>Reset</button>
        </div>
      </div>
    </CollapsibleCard>
  {:else}
    <CollapsibleCard title="Native Server Connection" bind:open={openSections.serverConnection}>
      <div class="server-row">
        <span class="server-pill" data-health={serverHealth}>
          {#if serverHealth === 'online'}
            Online on {activeServerUrl}
          {:else if serverHealth === 'checking'}
            Checking {activeServerUrl}...
          {:else}
            Offline on {activeServerUrl}
          {/if}
        </span>
        <button type="button" class="secondary-btn" onclick={() => pingServer()} disabled={isBusy}>
          Test
        </button>
      </div>

      <label class="field">
        <span>Listen Port</span>
        <div class="port-input-wrap">
          <span class="port-prefix">ws://127.0.0.1:</span>
          <input
            type="number"
            min="1"
            max="65535"
            inputmode="numeric"
            bind:value={portInput}
            disabled={isBusy || deleting}
            oninput={() => {
              portError = null;
              portSavedMsg = '';
            }}
            onkeydown={(e) => e.key === 'Enter' && applyPort(portInput)}
          />
        </div>
      </label>

      <div class="port-actions">
        <button
          type="button"
          class="secondary-btn"
          disabled={isBusy || deleting}
          onclick={() => applyPort(portInput)}
        >
          Apply Port
        </button>
        <button
          type="button"
          class="secondary-btn"
          disabled={isBusy || deleting || activePort === DEFAULT_SERVER_PORT}
          onclick={resetPort}
        >
          Reset ({DEFAULT_SERVER_PORT})
        </button>
        {#if portSavedMsg}
          <span class="pill-ok">{portSavedMsg}</span>
        {/if}
      </div>

      {#if portError}
        <small class="port-error">{portError}</small>
      {/if}

      {#if activePort === DEFAULT_SERVER_PORT}
        <small class="hint">Default port ({DEFAULT_SERVER_PORT}).</small>
      {:else}
        <small class="hint path-hint">
          Match this port in desktop app Settings (or launch with <code>LIVE_SUBTITLES_ADDR=127.0.0.1:{activePort}</code>).
        </small>
      {/if}
    </CollapsibleCard>

    <CollapsibleCard
      title="Model List"
      meta={`${visibleModels.length} shown`}
      bind:open={openSections.modelList}
    >
      <div class="catalog-list">
        {#each BUILTIN_MODELS as model (model.id)}
          {@const isVisible = !hiddenModelIds.includes(model.id)}
          <label class="catalog-row">
            <div class="catalog-info">
              <span class="downloaded-name">{model.label}</span>
              <span class="catalog-desc">{model.description}</span>
            </div>
            <input
              type="checkbox"
              class="catalog-toggle"
              checked={isVisible}
              disabled={isBusy || deleting || (isVisible && visibleModels.length <= 1)}
              onchange={() => toggleBuiltinVisibility(model.id)}
            />
          </label>
        {/each}
      </div>

      {#if customModels.length > 0}
        <div class="catalog-subhead">Custom Models</div>
        <div class="catalog-list">
          {#each customModels as custom (custom.id)}
            <div class="catalog-row">
              <div class="catalog-info">
                <span class="downloaded-name">{custom.label}</span>
                <span class="downloaded-sub">
                  {custom.repo ? `${custom.repo} · ` : ''}{custom.filename}
                </span>
              </div>
              <button
                type="button"
                class="secondary-btn danger"
                disabled={isBusy || deleting}
                onclick={() => handleRemoveCustomModel(custom.id)}
              >
                Remove
              </button>
            </div>
          {/each}
        </div>
      {/if}

      <div class="catalog-subhead">Add Custom Model</div>
      <div class="catalog-form">
        <input
          type="text"
          bind:value={newCustomLabel}
          placeholder="Display name (optional)"
          disabled={isBusy || deleting}
        />
        <input
          type="text"
          bind:value={newCustomDesc}
          placeholder="Good for... (optional)"
          disabled={isBusy || deleting}
        />
        <input
          type="text"
          bind:value={newCustomRepo}
          placeholder="HF repo, e.g. ggerganov/whisper.cpp (optional)"
          disabled={isBusy || deleting}
        />
        <input
          type="text"
          bind:value={newCustomFilename}
          placeholder="Filename (.bin), e.g. ggml-large-v3-q8_0.bin"
          disabled={isBusy || deleting}
          onkeydown={(e) => e.key === 'Enter' && handleAddCustomModel()}
        />

        {#if unlistedLocalFiles.length > 0}
          <div class="chips">
            {#each unlistedLocalFiles as file (file.filename)}
              <button
                type="button"
                class="chip"
                data-selected={newCustomFilename.trim() === file.filename}
                disabled={isBusy || deleting}
                onclick={() => (newCustomFilename = file.filename)}
              >
                {file.filename} ({formatBytes(file.bytes)})
              </button>
            {/each}
          </div>
        {/if}

        <div class="catalog-actions">
          <button
            type="button"
            class="secondary-btn"
            disabled={isBusy || deleting || !newCustomFilename.trim()}
            onclick={handleAddCustomModel}
          >
            Add to List
          </button>
          {#if catalogError}
            <small class="port-error">{catalogError}</small>
          {/if}
        </div>
      </div>
    </CollapsibleCard>

    <CollapsibleCard
      title="Downloaded Models"
      meta={downloadedModelsMeta}
      bind:open={openSections.downloadedModels}
    >
      {#if serverHealth !== 'online'}
        <span class="hint">Start native server to view or delete downloaded models.</span>
      {:else if localFileList.length === 0}
        <span class="hint">No models downloaded yet.</span>
      {:else}
        <div class="downloaded-list">
          {#each localFileList as file (file.filename)}
            {@const presetLabel = filenameLabelMap.get(file.filename)}
            <div class="downloaded-row">
              <div class="downloaded-info">
                <span class="downloaded-name" title={file.filename}>
                  {presetLabel ?? file.filename}
                </span>
                <span class="downloaded-sub">
                  {#if presetLabel}
                    {file.filename} · {formatBytes(file.bytes)}
                  {:else}
                    {formatBytes(file.bytes)}
                  {/if}
                </span>
              </div>
              <button
                type="button"
                class="secondary-btn danger"
                disabled={isBusy || deleting}
                onclick={() => handleDelete(file.filename)}
              >
                {deletingFilename === file.filename ? 'Deleting...' : 'Delete'}
              </button>
            </div>
          {/each}
        </div>
      {/if}

      {#if nativeCacheDir}
        <small class="hint path-hint">Folder: {nativeCacheDir}</small>
      {/if}
    </CollapsibleCard>
  {/if}

  {#if showProgress}
    <div class="progress-wrap">
      <div class="progress-bar-row">
        <div class="progress-bar">
          <div
            class="progress-fill"
            style={`width: ${activeProgress ?? 15}%`}
          ></div>
        </div>
        {#if isDownloading}
          <button
            type="button"
            class="secondary-btn danger cancel-dl-btn"
            onclick={handleCancelDownload}
          >
            Cancel
          </button>
        {/if}
      </div>
      <div class="progress-text">
        <span class="progress-label">{activeProgressLabel || 'Preparing...'}</span>
        {#if activeSpeedLabel || activeProgress !== null}
          <span class="progress-meta">
            {#if activeSpeedLabel}
              <span class="progress-speed">{activeSpeedLabel}</span>
            {/if}
            {#if activeProgress !== null}
              <span>{activeProgress}%</span>
            {/if}
          </span>
        {/if}
      </div>
    </div>
  {/if}

  {#if displayError}
    <p class="error">{displayError}</p>
  {/if}

  <button
    class="action-btn"
    data-stop={isCapturingState}
    data-blocked={startBlocked}
    aria-disabled={startBlocked || deleting || isDownloading}
    disabled={deleting || isDownloading}
    onclick={isCapturingState ? handleStop : handleStart}
  >
    {isCapturingState ? 'Stop Subtitles' : 'Start Subtitles on Active Tab'}
  </button>
</main>

<style>
  :global(body) {
    margin: 0;
    background: #111318;
    color: #eef1f6;
    font-family: system-ui, -apple-system, sans-serif;
  }

  .panel {
    width: 356px;
    padding: 14px;
    display: flex;
    flex-direction: column;
    gap: 10px;
    box-sizing: border-box;
  }

  .header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
  }

  .title-wrap {
    display: inline-flex;
    align-items: baseline;
    gap: 6px;
  }

  h1 {
    margin: 0;
    font-size: 15px;
    font-weight: 600;
  }

  .version-tag {
    font-size: 11px;
    font-weight: 500;
    color: #8c95a6;
    font-variant-numeric: tabular-nums;
  }

  .badge {
    font-size: 11px;
    padding: 3px 8px;
    border-radius: 999px;
    background: #232734;
    color: #b4bccb;
  }

  .badge[data-status='listening'] {
    background: #163826;
    color: #6ee7a8;
  }

  .badge[data-status='loading'] {
    background: #352b14;
    color: #fcd34d;
  }

  .badge[data-status='error'] {
    background: #3c191d;
    color: #fda4af;
  }

  .tab-bar {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: 4px;
    padding: 3px;
    border-radius: 7px;
    border: 1px solid #252b3b;
    background: #151821;
  }

  .tab-btn {
    padding: 6px 8px;
    border: none;
    border-radius: 5px;
    background: transparent;
    color: #9ba3b4;
    font-size: 12px;
    font-weight: 500;
    cursor: pointer;
  }

  .tab-btn[data-active='true'] {
    background: #233049;
    color: #ffffff;
    font-weight: 600;
  }

  .downloaded-list {
    max-height: 210px;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .downloaded-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    padding: 6px 8px;
    border-radius: 6px;
    border: 1px solid #222838;
    background: #11151d;
  }

  .downloaded-info {
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .downloaded-name {
    font-size: 11px;
    font-weight: 500;
    color: #e2e8f0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .downloaded-sub {
    font-size: 10px;
    color: #8c95a6;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-variant-numeric: tabular-nums;
  }

  .server-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
  }

  .server-pill {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: 11px;
    font-weight: 500;
    color: #8c95a6;
    line-height: 1.3;
  }

  .server-dot {
    width: 7px;
    height: 7px;
    border-radius: 999px;
    background: #8c95a6;
    flex-shrink: 0;
  }

  .server-pill[data-health='online'] {
    color: #6ee7a8;
  }

  .server-pill[data-health='online'] .server-dot {
    background: #4ade80;
  }

  .server-pill[data-health='offline'] {
    color: #fca5a5;
  }

  .server-pill[data-health='offline'] .server-dot {
    background: #f87171;
  }

  .field {
    display: flex;
    flex-direction: column;
    gap: 4px;
    font-size: 12px;
    color: #b4bccb;
  }

  .dropdown {
    position: relative;
  }

  .dropdown-trigger {
    width: 100%;
    padding: 7px 9px;
    border-radius: 6px;
    border: 1px solid #2b3140;
    background: #181b22;
    color: #eef1f6;
    font-size: 12px;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    cursor: pointer;
    text-align: left;
  }

  .dropdown-trigger:disabled {
    opacity: 0.6;
    cursor: not-allowed;
  }

  .trigger-label {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .trigger-meta {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-shrink: 0;
  }

  .caret {
    font-size: 9px;
    color: #8c95a6;
  }

  .dropdown-menu {
    margin-top: 4px;
    border-radius: 7px;
    border: 1px solid #30384c;
    background: #141821;
    overflow: hidden;
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.45);
  }

  .option-list {
    max-height: 236px;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
  }

  .option-row {
    width: 100%;
    padding: 7px 9px;
    border: none;
    border-bottom: 1px solid #1e2330;
    background: transparent;
    color: #dbe2f0;
    font-size: 11px;
    display: flex;
    flex-direction: column;
    align-items: stretch;
    gap: 2px;
    text-align: left;
    cursor: pointer;
  }

  .option-row:hover {
    background: #1d2434;
  }

  .option-row[data-selected='true'] {
    background: #233049;
    color: #ffffff;
    font-weight: 600;
  }

  .opt-main {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
  }

  .opt-name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .opt-right {
    display: flex;
    align-items: center;
    gap: 5px;
    flex-shrink: 0;
  }

  .opt-desc {
    font-size: 10px;
    font-weight: 400;
    color: #94a3b8;
    line-height: 1.3;
  }

  .pill-ok {
    font-size: 10px;
    padding: 1px 6px;
    border-radius: 999px;
    background: #163826;
    color: #6ee7a8;
  }

  .pill-size {
    font-size: 10px;
    color: #8c95a6;
  }

  .quant-field {
    display: flex;
    flex-direction: column;
    gap: 5px;
    padding: 8px 9px;
    border-radius: 6px;
    border: 1px solid #252b3b;
    background: #151821;
  }

  .quant-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    font-size: 12px;
    color: #b4bccb;
  }

  .quant-field input[type='range'] {
    width: 100%;
    margin: 0;
    accent-color: #3b82f6;
    cursor: pointer;
  }

  .quant-field input[type='range']:disabled {
    opacity: 0.6;
    cursor: not-allowed;
  }

  .quant-scale {
    display: flex;
    justify-content: space-between;
    font-size: 10px;
    color: #8c95a6;
  }

  .catalog-list {
    max-height: 185px;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 5px;
  }

  .catalog-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    padding: 6px 8px;
    border-radius: 6px;
    border: 1px solid #222838;
    background: #11151d;
    cursor: pointer;
  }

  .catalog-info {
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .catalog-desc {
    font-size: 10px;
    color: #8c95a6;
    line-height: 1.3;
  }

  .catalog-toggle {
    width: 15px;
    height: 15px;
    flex-shrink: 0;
    accent-color: #3b82f6;
    cursor: pointer;
  }

  .catalog-subhead {
    font-size: 10px;
    font-weight: 600;
    color: #93c5fd;
    text-transform: uppercase;
    letter-spacing: 0.03em;
    margin-top: 2px;
  }

  .catalog-form {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .catalog-actions {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
  }

  select,
  input[type='text'] {
    width: 100%;
    padding: 7px 9px;
    border-radius: 6px;
    border: 1px solid #2b3140;
    background: #181b22;
    color: #eef1f6;
    font-size: 12px;
    box-sizing: border-box;
  }

  select:disabled,
  input[type='text']:disabled {
    opacity: 0.6;
  }

  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
  }

  .chip {
    padding: 3px 7px;
    border-radius: 5px;
    border: 1px solid #2b3140;
    background: #161a22;
    color: #cbd5e1;
    font-size: 10px;
    cursor: pointer;
  }

  .chip[data-selected='true'] {
    border-color: #60a5fa;
    background: #22304a;
    color: #fff;
  }

  .hint {
    color: #8c95a6;
    font-size: 11px;
    line-height: 1.3;
  }

  .toggle-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    font-size: 12px;
    color: #b4bccb;
    cursor: pointer;
  }

  .toggle-row input[type='checkbox'] {
    width: 15px;
    height: 15px;
    accent-color: #3b82f6;
    cursor: pointer;
  }

  .style-panel {
    display: flex;
    flex-direction: column;
    gap: 7px;
  }

  .port-input-wrap {
    display: flex;
    align-items: center;
    border: 1px solid #2b3140;
    border-radius: 6px;
    background: #181b22;
    overflow: hidden;
  }

  .port-prefix {
    padding: 7px 6px 7px 9px;
    font-family: ui-monospace, SFMono-Regular, monospace;
    font-size: 11px;
    color: #8c95a6;
    background: #14171f;
    border-right: 1px solid #252b3b;
    white-space: nowrap;
  }

  .port-input-wrap input[type='number'] {
    width: 100%;
    padding: 7px 9px;
    border: none;
    background: transparent;
    color: #eef1f6;
    font-family: ui-monospace, SFMono-Regular, monospace;
    font-size: 12px;
    box-sizing: border-box;
    outline: none;
  }

  .port-input-wrap input[type='number']:disabled {
    opacity: 0.6;
  }

  .port-actions {
    display: flex;
    align-items: center;
    gap: 6px;
  }

  .port-error {
    color: #fca5a5;
    font-size: 11px;
  }

  code {
    font-family: ui-monospace, SFMono-Regular, monospace;
    font-size: 10px;
    color: #cbd5e1;
  }

  .style-panel input[type='range'] {
    width: 100%;
    margin: 0;
    accent-color: #3b82f6;
    cursor: pointer;
  }

  .style-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    font-size: 11px;
    color: #b4bccb;
  }

  .style-val {
    color: #93c5fd;
    font-variant-numeric: tabular-nums;
  }

  .color-grid {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 8px;
  }

  .color-field {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 6px;
    padding: 4px 7px;
    border: 1px solid #2b3140;
    border-radius: 6px;
    background: #181b22;
    font-size: 11px;
    color: #b4bccb;
    cursor: pointer;
  }

  .color-field input[type='color'] {
    width: 24px;
    height: 20px;
    padding: 0;
    border: none;
    background: transparent;
    cursor: pointer;
  }

  .style-preview-wrap {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    margin-top: 2px;
  }

  .style-sample {
    padding: 4px 10px;
    border-radius: 6px;
    font-weight: 600;
    line-height: 1.3;
    text-shadow: 0 1px 3px rgba(0, 0, 0, 0.9);
  }

  .path-hint {
    word-break: break-all;
  }

  .cache-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
  }

  .secondary-btn {
    flex-shrink: 0;
    padding: 4px 9px;
    border-radius: 5px;
    border: 1px solid #2b3140;
    background: #1b202b;
    color: #dbe2f0;
    font-size: 11px;
    font-weight: 500;
    cursor: pointer;
  }

  .secondary-btn.danger {
    border-color: #5b262d;
    color: #fda4af;
  }

  .secondary-btn:disabled,
  .action-btn:disabled,
  .action-btn[data-blocked='true'] {
    opacity: 0.55;
    cursor: not-allowed;
  }

  .progress-wrap {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .progress-bar-row {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .progress-bar {
    flex: 1;
    height: 6px;
    background: #232734;
    border-radius: 999px;
    overflow: hidden;
  }

  .cancel-dl-btn {
    padding: 2px 7px;
    font-size: 10px;
  }

  .progress-fill {
    height: 100%;
    background: #60a5fa;
    transition: width 0.2s ease;
  }

  .progress-text {
    display: flex;
    justify-content: space-between;
    gap: 8px;
    font-size: 11px;
    color: #9ba3b4;
  }

  .progress-label {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .progress-meta {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    flex-shrink: 0;
    font-variant-numeric: tabular-nums;
  }

  .progress-speed {
    color: #93c5fd;
  }

  .error {
    margin: 0;
    padding: 8px 10px;
    border-radius: 6px;
    background: #3c191d;
    color: #fecdd3;
    font-size: 12px;
    line-height: 1.35;
  }

  .action-btn {
    width: 100%;
    padding: 9px 12px;
    border: none;
    border-radius: 7px;
    background: #3b82f6;
    color: #fff;
    font-size: 13px;
    font-weight: 600;
    cursor: pointer;
  }

  .action-btn[data-stop='true'] {
    background: #dc2626;
  }
</style>
