import type { ElevenLabsModelInfo } from './elevenlabs';
import type { GoogleModelInfo } from './gemini';
import type { CustomModelDefinition, NativeModelSource, TargetLanguage } from './models';

export type SessionStatus = 'idle' | 'loading' | 'listening' | 'error';
export const CLOUD_PROVIDERS = [
  { id: 'google', label: 'Google Gemini' },
  { id: 'soniox', label: 'Soniox' },
  { id: 'elevenlabs', label: 'ElevenLabs' },
] as const;
export type CloudProvider = (typeof CLOUD_PROVIDERS)[number]['id'];
export type SpeechProvider = Exclude<CloudProvider, 'google'>;
export type ProviderKind = 'local' | CloudProvider;
export type DeviceKind = 'vulkan' | 'metal' | CloudProvider | null;
export type TranslatorEngine = 'disabled' | 'hy-mt3';
export type VadModel = 'silero-v5' | 'rms';
export type EndpointMechanism = 'silence-balanced' | 'silence-fast' | 'fixed-window';
export type DiarizationModel = 'disabled' | 'nemotron-sortformer';

export const DEFAULT_TRANSLATOR_ENGINE: TranslatorEngine = 'disabled';
export const DEFAULT_VAD_MODEL: VadModel = 'silero-v5';
export const DEFAULT_ENDPOINT_MECHANISM: EndpointMechanism = 'silence-balanced';
export const DEFAULT_DIARIZATION_MODEL: DiarizationModel = 'disabled';

export const VAD_MODELS: readonly { id: VadModel; label: string }[] = [
  { id: 'silero-v5', label: 'Silero VAD v5 (Neural)' },
  { id: 'rms', label: 'RMS Energy (Lightweight)' },
];

export const ENDPOINT_MECHANISMS: readonly { id: EndpointMechanism; label: string }[] = [
  { id: 'silence-balanced', label: 'Silence Pause (Balanced)' },
  { id: 'silence-fast', label: 'Silence Pause (Fast)' },
  { id: 'fixed-window', label: 'Fixed Window (3s)' },
];

export const DIARIZATION_MODELS: readonly { id: DiarizationModel; label: string }[] = [
  { id: 'disabled', label: 'Disabled' },
  { id: 'nemotron-sortformer', label: 'Nemotron-3 Sortformer (Streaming)' },
];

export function sanitizeTranslatorEngine(raw: unknown): TranslatorEngine {
  return raw === 'hy-mt3' ? 'hy-mt3' : 'disabled';
}

export function sanitizeVadModel(raw: unknown): VadModel {
  return raw === 'rms' ? 'rms' : 'silero-v5';
}

export function sanitizeEndpointMechanism(raw: unknown): EndpointMechanism {
  if (raw === 'silence-fast' || raw === 'fixed-window') {
    return raw;
  }
  return 'silence-balanced';
}

export function sanitizeDiarizationModel(
  raw: unknown,
  legacyEnabled = false,
): DiarizationModel {
  if (raw === 'nemotron-sortformer') return 'nemotron-sortformer';
  if (raw === 'disabled') return 'disabled';
  return legacyEnabled ? 'nemotron-sortformer' : 'disabled';
}

export function sanitizeProvider(raw: unknown): ProviderKind {
  return CLOUD_PROVIDERS.find(({ id }) => id === raw)?.id ?? 'local';
}

export interface FuriganaToken {
  text: string;
  reading?: string;
}

export interface SubtitleStyle {
  fontSize: number;
  textColor: string;
  bgColor: string;
  bgOpacity: number;
}

export const DEFAULT_SUBTITLE_STYLE: SubtitleStyle = {
  fontSize: 24,
  textColor: '#ffffff',
  bgColor: '#000000',
  bgOpacity: 78,
};

function isHexColor(value: unknown): value is string {
  return typeof value === 'string' && /^#[0-9a-fA-F]{6}$/.test(value);
}

export function sanitizeSubtitleStyle(raw?: Partial<SubtitleStyle> | null): SubtitleStyle {
  const fontSize =
    typeof raw?.fontSize === 'number' && Number.isFinite(raw.fontSize)
      ? Math.round(Math.min(56, Math.max(14, raw.fontSize)))
      : DEFAULT_SUBTITLE_STYLE.fontSize;
  const bgOpacity =
    typeof raw?.bgOpacity === 'number' && Number.isFinite(raw.bgOpacity)
      ? Math.round(Math.min(100, Math.max(0, raw.bgOpacity)))
      : DEFAULT_SUBTITLE_STYLE.bgOpacity;

  return {
    fontSize,
    textColor: isHexColor(raw?.textColor) ? raw.textColor : DEFAULT_SUBTITLE_STYLE.textColor,
    bgColor: isHexColor(raw?.bgColor) ? raw.bgColor : DEFAULT_SUBTITLE_STYLE.bgColor,
    bgOpacity,
  };
}

export function hexToRgba(hex: string, opacityPct: number): string {
  const clean = isHexColor(hex) ? hex : '#000000';
  const r = Number.parseInt(clean.slice(1, 3), 16);
  const g = Number.parseInt(clean.slice(3, 5), 16);
  const b = Number.parseInt(clean.slice(5, 7), 16);
  const alpha = +(Math.min(100, Math.max(0, opacityPct)) / 100).toFixed(2);
  return `rgba(${r}, ${g}, ${b}, ${alpha})`;
}

export const SPEAKER_COLORS = [
  '#38bdf8',
  '#fbbf24',
  '#fb7185',
  '#34d399',
  '#a78bfa',
  '#fb923c',
  '#2dd4bf',
  '#f472b6',
] as const;

export function getSpeakerColor(speaker: number): string {
  if (!Number.isFinite(speaker) || speaker < 1) {
    return SPEAKER_COLORS[0];
  }
  const idx = (Math.floor(speaker) - 1) % SPEAKER_COLORS.length;
  return SPEAKER_COLORS[idx] ?? SPEAKER_COLORS[0];
}

export const DEFAULT_SERVER_PORT = 8765;

export function sanitizePort(raw: unknown): number | null {
  if (typeof raw === 'number') {
    if (!Number.isInteger(raw) || raw < 1 || raw > 65535) return null;
    return raw;
  }
  if (typeof raw === 'string') {
    const trimmed = raw.trim();
    if (!/^\d+$/.test(trimmed)) return null;
    const parsed = Number.parseInt(trimmed, 10);
    if (!Number.isInteger(parsed) || parsed < 1 || parsed > 65535) return null;
    return parsed;
  }
  return null;
}

export function portToServerUrl(port: number): string {
  return `ws://127.0.0.1:${sanitizePort(port) ?? DEFAULT_SERVER_PORT}`;
}

export function parsePortFromServerUrl(url: string): number {
  try {
    const parsed = new URL(url);
    return sanitizePort(parsed.port) ?? DEFAULT_SERVER_PORT;
  } catch {
    return DEFAULT_SERVER_PORT;
  }
}

export interface ExtensionState {
  status: SessionStatus;
  provider: ProviderKind;
  tabId: number | null;
  modelId: string;
  quantId: string;
  hiddenModelIds: string[];
  customModels: CustomModelDefinition[];
  geminiApiKey: string;
  speechApiKeys: Record<SpeechProvider, string>;
  googleModelId: string;
  googleModels: GoogleModelInfo[];
  elevenLabsModelId: string;
  elevenLabsModels: ElevenLabsModelInfo[];
  googleTargetLanguage: string;
  translatorEngine: TranslatorEngine;
  translatorTargetLanguage: string;
  enableVadEndpointing: boolean;
  vadModel: VadModel;
  endpointMechanism: EndpointMechanism;
  diarizationModel: DiarizationModel;
  language: string;
  targetLanguage: TargetLanguage;
  showFurigana: boolean;
  enableDiarization: boolean;
  subtitleStyle: SubtitleStyle;
  serverUrl: string;
  device: DeviceKind;
  activity: string;
  progress: number | null;
  progressLabel: string;
  progressSpeedBps: number | null;
  downloadingFilename: string | null;
  downloadProgress: number | null;
  downloadLabel: string;
  downloadSpeedBps: number | null;
  error: string | null;
  lastText: string;
}

export type PopupToBackgroundMessage =
  | { type: 'GET_STATE' }
  | {
      type: 'START_CAPTURE';
      provider?: ProviderKind;
      tabId: number;
      modelId: string;
      quantId: string;
      source?: NativeModelSource;
      googleModel?: GoogleModelInfo;
      elevenLabsModelId?: string;
      language: string;
      targetLanguage?: TargetLanguage;
      googleTargetLanguage?: string;
      translatorEngine?: TranslatorEngine;
      translatorTargetLanguage?: string;
      enableVadEndpointing?: boolean;
      vadModel?: VadModel;
      endpointMechanism?: EndpointMechanism;
      diarizationModel?: DiarizationModel;
      enableDiarization?: boolean;
      serverUrl?: string;
    }
  | { type: 'STOP_CAPTURE' }
  | { type: 'SET_PROVIDER'; provider: ProviderKind }
  | {
      type: 'SET_GEMINI_API_KEY';
      apiKey: string;
      models?: GoogleModelInfo[];
      googleModelId?: string;
    }
  | { type: 'SET_SPEECH_API_KEY'; provider: SpeechProvider; apiKey: string }
  | {
      type: 'SET_GOOGLE_MODELS';
      models: GoogleModelInfo[];
      googleModelId?: string;
    }
  | { type: 'SET_GOOGLE_MODEL'; googleModelId: string }
  | { type: 'REFRESH_ELEVENLABS_MODELS' }
  | { type: 'SET_ELEVENLABS_MODEL'; modelId: string }
  | { type: 'SET_LANGUAGE'; language: string }
  | { type: 'SET_TARGET_LANGUAGE'; targetLanguage: TargetLanguage }
  | { type: 'SET_GOOGLE_TARGET_LANGUAGE'; targetLanguage: string }
  | { type: 'SET_TRANSLATOR_ENGINE'; translatorEngine: TranslatorEngine }
  | { type: 'SET_TRANSLATOR_TARGET_LANGUAGE'; targetLanguage: string }
  | { type: 'SET_VAD_ENDPOINTING'; enabled: boolean }
  | { type: 'SET_VAD_MODEL'; vadModel: VadModel }
  | { type: 'SET_ENDPOINT_MECHANISM'; endpointMechanism: EndpointMechanism }
  | { type: 'SET_DIARIZATION_MODEL'; diarizationModel: DiarizationModel }
  | { type: 'SET_FURIGANA'; enabled: boolean }
  | { type: 'SET_STYLE'; style: SubtitleStyle }
  | { type: 'SET_SERVER_URL'; serverUrl: string }
  | {
      type: 'SET_MODEL_CATALOG';
      hiddenModelIds: string[];
      customModels: CustomModelDefinition[];
    }
  | {
      type: 'DOWNLOAD_MODEL';
      source: NativeModelSource;
      serverUrl?: string;
    }
  | {
      type: 'DOWNLOAD_TRANSLATOR';
      serverUrl?: string;
    }
  | { type: 'CANCEL_DOWNLOAD' };

export type BackgroundToOffscreenMessage =
  | {
      type: 'OFFSCREEN_START';
      provider: 'local';
      streamId: string;
      source: NativeModelSource;
      language: string;
      translate: boolean;
      translatorEngine: TranslatorEngine;
      translatorTargetLanguage: string;
      enableVadEndpointing: boolean;
      vadModel: VadModel;
      endpointMechanism: EndpointMechanism;
      diarizationModel: DiarizationModel;
      enableDiarization: boolean;
      serverUrl: string;
    }
  | {
      type: 'OFFSCREEN_START';
      provider: 'google';
      streamId: string;
      apiKey: string;
      model: GoogleModelInfo;
      language: string;
      targetLanguage: string;
      translatorEngine: TranslatorEngine;
      translatorTargetLanguage: string;
      enableVadEndpointing: boolean;
      vadModel: VadModel;
      endpointMechanism: EndpointMechanism;
      diarizationModel: DiarizationModel;
      enableDiarization: boolean;
      serverUrl: string;
    }
  | {
      type: 'OFFSCREEN_START';
      provider: SpeechProvider;
      streamId: string;
      apiKey: string;
      elevenLabsModelId?: string;
      language: string;
      translatorEngine: TranslatorEngine;
      translatorTargetLanguage: string;
      diarizationModel: DiarizationModel;
      enableDiarization: boolean;
      serverUrl: string;
    }
  | { type: 'OFFSCREEN_SET_DIARIZATION'; enabled: boolean; model?: DiarizationModel }
  | { type: 'OFFSCREEN_STOP' };

export const MAX_OVERLAY_LINES = 3;
export const LINE_EXPIRE_MS = 8000;

export interface SubtitleLine {
  id: number;
  text: string;
  tokens?: FuriganaToken[];
  speakers?: number[];
  isFinal: boolean;
  updatedAt: number;
}

export function pushSubtitleLine(
  lines: SubtitleLine[],
  incoming: Omit<SubtitleLine, 'id'>,
  nextId: number,
  maxLines = MAX_OVERLAY_LINES,
  expireMs = LINE_EXPIRE_MS,
): { lines: SubtitleLine[]; nextId: number } {
  const active = lines.filter(
    (line) => !line.isFinal || incoming.updatedAt - line.updatedAt <= expireMs,
  );
  const last = active[active.length - 1];
  if (!incoming.text.trim()) {
    return { lines: last && !last.isFinal ? active.slice(0, -1) : active, nextId };
  }
  let updated: SubtitleLine[];
  let outNextId = nextId;

  if (last && !last.isFinal) {
    updated = [...active.slice(0, -1), { ...incoming, id: last.id }];
  } else {
    updated = [...active, { ...incoming, id: nextId }];
    outNextId = nextId + 1;
  }

  if (updated.length > maxLines) {
    updated = updated.slice(updated.length - maxLines);
  }
  return { lines: updated, nextId: outNextId };
}

export type OffscreenToBackgroundMessage =
  | {
      type: 'OFFSCREEN_STATUS';
      status: SessionStatus;
      device?: DeviceKind;
      activity?: string;
      progress?: number | null;
      progressLabel?: string;
      speedBps?: number | null;
      error?: string | null;
    }
  | {
      type: 'OFFSCREEN_TRANSCRIPT';
      text: string;
      tokens?: FuriganaToken[];
      speakers?: number[];
      isFinal: boolean;
    };

export interface SubtitleView {
  lines: SubtitleLine[];
  showFurigana?: boolean;
  style?: SubtitleStyle;
  visible: boolean;
}

export type BackgroundToContentMessage = SubtitleView & {
  type: 'SUBTITLE_UPDATE';
};

export type BackgroundBroadcastMessage = {
  type: 'STATE_CHANGED';
  state: ExtensionState;
};

export type RuntimeMessage =
  | PopupToBackgroundMessage
  | BackgroundToOffscreenMessage
  | OffscreenToBackgroundMessage
  | BackgroundToContentMessage
  | BackgroundBroadcastMessage;
