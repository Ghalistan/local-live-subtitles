import type { CustomModelDefinition, NativeModelSource, TargetLanguage } from './models';

export type SessionStatus = 'idle' | 'loading' | 'listening' | 'error';
export type DeviceKind = 'vulkan' | 'metal' | null;

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
  tabId: number | null;
  modelId: string;
  quantId: string;
  hiddenModelIds: string[];
  customModels: CustomModelDefinition[];
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
      tabId: number;
      modelId: string;
      quantId: string;
      source: NativeModelSource;
      language: string;
      targetLanguage?: TargetLanguage;
      enableDiarization?: boolean;
      serverUrl?: string;
    }
  | { type: 'STOP_CAPTURE' }
  | { type: 'SET_FURIGANA'; enabled: boolean }
  | { type: 'SET_DIARIZATION'; enabled: boolean }
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
  | { type: 'CANCEL_DOWNLOAD' };

export type BackgroundToOffscreenMessage =
  | {
      type: 'OFFSCREEN_START';
      streamId: string;
      source: NativeModelSource;
      language: string;
      translate: boolean;
      enableDiarization: boolean;
      serverUrl: string;
    }
  | { type: 'OFFSCREEN_SET_DIARIZATION'; enabled: boolean }
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
