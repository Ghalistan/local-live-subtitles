export type GoogleModelTransport = 'live' | 'generate';
export type GoogleModelKind =
  | 'transcribe-live'
  | 'translate-live'
  | 'live'
  | 'transcribe'
  | 'multimodal';

export interface GoogleModelInfo {
  id: string;
  name: string;
  displayName: string;
  description: string;
  supportedGenerationMethods: string[];
  transport: GoogleModelTransport;
  kind: GoogleModelKind;
}

export const GOOGLE_TARGET_LANGUAGES: readonly { code: string; label: string }[] = [
  { code: 'same', label: 'Same as spoken (Transcribe)' },
  { code: 'en', label: 'English' },
  { code: 'ja', label: 'Japanese' },
  { code: 'id', label: 'Indonesian' },
  { code: 'zh', label: 'Chinese' },
  { code: 'ko', label: 'Korean' },
  { code: 'es', label: 'Spanish' },
  { code: 'fr', label: 'French' },
  { code: 'de', label: 'German' },
];

export const GOOGLE_TRANSLATE_ONLY_TARGETS: readonly { code: string; label: string }[] =
  GOOGLE_TARGET_LANGUAGES.filter((entry) => entry.code !== 'same');

export const DEFAULT_GOOGLE_TARGET_LANGUAGE = 'same';
export const DEFAULT_TRANSLATOR_TARGET_LANGUAGE = 'en';

const VALID_TARGET_CODES = new Set(GOOGLE_TARGET_LANGUAGES.map((t) => t.code));
const VALID_TRANSLATOR_TARGET_CODES = new Set(
  GOOGLE_TRANSLATE_ONLY_TARGETS.map((t) => t.code),
);

export function sanitizeGoogleTargetLanguage(raw: unknown): string {
  if (typeof raw === 'string' && VALID_TARGET_CODES.has(raw)) {
    return raw;
  }
  return DEFAULT_GOOGLE_TARGET_LANGUAGE;
}

export function sanitizeTranslatorTargetLanguage(raw: unknown): string {
  if (typeof raw === 'string' && VALID_TRANSLATOR_TARGET_CODES.has(raw)) {
    return raw;
  }
  return DEFAULT_TRANSLATOR_TARGET_LANGUAGE;
}

export function getGoogleModelBadge(kind: GoogleModelKind): string {
  switch (kind) {
    case 'transcribe-live':
      return 'Live STT';
    case 'translate-live':
      return 'Live Translate';
    case 'live':
      return 'Live';
    case 'transcribe':
      return 'Transcribe';
    case 'multimodal':
      return 'Audio';
  }
}

export function googleModelSupportsDirectS2tt(kind: GoogleModelKind): boolean {
  return kind !== 'transcribe-live' && kind !== 'transcribe';
}

export function googleModelSupportsTargetLanguage(kind: GoogleModelKind): boolean {
  return googleModelSupportsDirectS2tt(kind);
}

const VALID_KINDS = new Set<GoogleModelKind>([
  'transcribe-live',
  'translate-live',
  'live',
  'transcribe',
  'multimodal',
]);

const KIND_PRIORITY: Record<GoogleModelKind, number> = {
  'transcribe-live': 0,
  'translate-live': 1,
  live: 2,
  transcribe: 3,
  multimodal: 4,
};

export function sanitizeGoogleModels(raw: unknown): GoogleModelInfo[] {
  if (!Array.isArray(raw)) return [];
  const models: GoogleModelInfo[] = [];
  const seenIds = new Set<string>();

  for (const item of raw) {
    if (!item || typeof item !== 'object') continue;
    const obj = item as Record<string, unknown>;
    const id = typeof obj.id === 'string' ? obj.id.trim().replace(/^models\//, '') : '';
    if (!id || seenIds.has(id)) continue;

    const methods = Array.isArray(obj.supportedGenerationMethods)
      ? obj.supportedGenerationMethods.filter((m): m is string => typeof m === 'string')
      : [];

    const transport: GoogleModelTransport | null =
      obj.transport === 'live' || obj.transport === 'generate' ? obj.transport : null;
    const kind: GoogleModelKind | null =
      typeof obj.kind === 'string' && VALID_KINDS.has(obj.kind as GoogleModelKind)
        ? (obj.kind as GoogleModelKind)
        : null;

    if (!transport || !kind) continue;

    seenIds.add(id);
    models.push({
      id,
      name: `models/${id}`,
      displayName:
        typeof obj.displayName === 'string' && obj.displayName.trim()
          ? obj.displayName.trim()
          : id,
      description: typeof obj.description === 'string' ? obj.description.trim() : '',
      supportedGenerationMethods: methods,
      transport,
      kind,
    });
  }

  models.sort((a, b) => {
    const prioDiff = KIND_PRIORITY[a.kind] - KIND_PRIORITY[b.kind];
    if (prioDiff !== 0) return prioDiff;
    return b.id.localeCompare(a.id, 'en', { numeric: true, sensitivity: 'base' });
  });
  return models;
}

export function pickDefaultGoogleModel(
  models: readonly GoogleModelInfo[],
  preferredId?: string,
): GoogleModelInfo | null {
  if (models.length === 0) return null;
  if (preferredId) {
    const exact = models.find((m) => m.id === preferredId);
    if (exact) return exact;
  }
  return models[0] ?? null;
}

export function fetchGoogleModelsViaServer(
  serverUrl: string,
  apiKey: string,
): Promise<GoogleModelInfo[]> {
  const trimmedKey = apiKey.trim();
  if (!trimmedKey) {
    return Promise.reject(new Error('Enter a Google Gemini API key first.'));
  }

  return new Promise((resolve, reject) => {
    let settled = false;
    let ws: WebSocket | null = null;

    const cleanup = () => {
      if (!ws) return;
      ws.onopen = null;
      ws.onmessage = null;
      ws.onerror = null;
      ws.onclose = null;
      if (ws.readyState === WebSocket.OPEN || ws.readyState === WebSocket.CONNECTING) {
        ws.close();
      }
      ws = null;
    };

    const timer = setTimeout(() => {
      if (settled) return;
      settled = true;
      cleanup();
      reject(new Error('Timed out waiting for native server to fetch Google models.'));
    }, 15000);

    try {
      ws = new WebSocket(serverUrl);
      ws.onopen = () => {
        ws?.send(JSON.stringify({ type: 'listGoogleModels', apiKey: trimmedKey }));
      };
      ws.onmessage = (ev) => {
        if (settled) return;
        try {
          const data = JSON.parse(String(ev.data)) as {
            type?: string;
            models?: unknown;
            error?: string;
          };
          if (data.type === 'googleModels') {
            settled = true;
            clearTimeout(timer);
            cleanup();
            resolve(sanitizeGoogleModels(data.models));
          } else if (data.type === 'error') {
            settled = true;
            clearTimeout(timer);
            cleanup();
            reject(new Error(data.error || 'Failed to fetch Google models.'));
          }
        } catch {
          // Ignore malformed frames
        }
      };
      ws.onerror = () => {
        if (settled) return;
        settled = true;
        clearTimeout(timer);
        cleanup();
        reject(new Error('Native server offline — start live-subtitles-native first.'));
      };
      ws.onclose = () => {
        if (settled) return;
        settled = true;
        clearTimeout(timer);
        cleanup();
        reject(new Error('Connection to native server closed unexpectedly.'));
      };
    } catch (err) {
      settled = true;
      clearTimeout(timer);
      cleanup();
      reject(err instanceof Error ? err : new Error('Failed to connect to native server.'));
    }
  });
}
