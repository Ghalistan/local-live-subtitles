export const DEFAULT_SERVER_URL = 'ws://127.0.0.1:8765';

export interface ModelQuantOption {
  id: string;
  label: string;
  filename: string;
  sizeLabel: string;
}

export type DiscoveredModelVariant = ModelQuantOption & { bytes: number };

export interface ModelEntry {
  id: string;
  label: string;
  description: string;
  repo?: string;
  filenameStem?: string;
  variants: readonly ModelQuantOption[];
  defaultVariantId: string;
  japaneseOnly?: boolean;
  supportsDirectS2tt?: boolean;
  isCustom?: boolean;
}

export interface CustomModelDefinition {
  id: string;
  label: string;
  description: string;
  repo?: string;
  filename: string;
  japaneseOnly?: boolean;
}

export interface NativeModelSource {
  filename: string;
  repo?: string;
  url?: string;
  japaneseOnly?: boolean;
  supportsDirectS2tt?: boolean;
}

export type TargetLanguage = 'same' | 'en';

export const DEFAULT_TARGET_LANGUAGE: TargetLanguage = 'same';

export const TARGET_LANGUAGES: readonly { code: TargetLanguage; label: string }[] = [
  { code: 'same', label: 'Same as spoken (Transcribe)' },
  { code: 'en', label: 'English (Direct S2TT)' },
];

export function sanitizeTargetLanguage(raw: unknown): TargetLanguage {
  return raw === 'en' ? 'en' : 'same';
}

export function inferCustomDirectS2tt(filename: string): boolean {
  const lower = filename.trim().toLowerCase();
  if (!lower) return false;
  if (lower.includes('kotoba') || lower.includes('turbo') || lower.includes('distil')) {
    return false;
  }
  if (/(?:^|[-_.])en(?:\.bin$|[-_])/.test(lower)) {
    return false;
  }
  return true;
}

export function isValidCustomFilename(filename: string): boolean {
  const name = filename.trim();
  return (
    name.length > 0 &&
    name !== '.bin' &&
    name.endsWith('.bin') &&
    !name.includes('/') &&
    !name.includes('\\') &&
    !name.includes('..') &&
    !name.includes('\0')
  );
}

export const BUILTIN_MODELS: readonly ModelEntry[] = [
  {
    id: 'whisper-large-v3',
    label: 'Whisper Large v3',
    description: 'Multilingual ASR & Direct S2TT to English (best all-round accuracy).',
    repo: 'ggerganov/whisper.cpp',
    filenameStem: 'ggml-large-v3',
    defaultVariantId: 'q5_0',
    supportsDirectS2tt: true,
    variants: [],
  },
  {
    id: 'kotoba-v2.2',
    label: 'Kotoba Whisper v2.2',
    description: 'Japanese VTubers, anime & casual slang (Japanese-only).',
    repo: 'Pomni/kotoba-whisper-v2.2-ggml-allquants',
    filenameStem: 'ggml-kotoba-v2.2',
    defaultVariantId: 'q8_0',
    japaneseOnly: true,
    supportsDirectS2tt: false,
    variants: [],
  },
];

type ModelFile = { filename: string; bytes: number };

export function parseModelVariants(
  model: ModelEntry,
  files: readonly ModelFile[],
): DiscoveredModelVariant[] {
  const stem = model.filenameStem;
  if (!stem) return [];
  const variants = new Map<string, DiscoveredModelVariant>();
  for (const file of files) {
    if (!file || typeof file.filename !== 'string') continue;
    if (!Number.isSafeInteger(file.bytes) || file.bytes <= 0) continue;
    const { filename, bytes } = file;
    if (!filename.startsWith(stem) || !isValidCustomFilename(filename)) continue;
    const suffix = filename.slice(stem.length, -4);
    // whisper.cpp's unsuffixed GGML release is FP16.
    const id = suffix === ''
      ? 'f16'
      : /^-(q\d+(?:_[a-z0-9]+)*|f16|f32)$/i.exec(suffix)?.[1]?.toLowerCase();
    if (!id || variants.has(id)) continue;
    const label = id.startsWith('f') ? 'FP' + id.slice(1) : id.toUpperCase();
    variants.set(id, { id, label, filename, bytes, sizeLabel: formatBytes(bytes) });
  }
  return [...variants.values()].sort((a, b) => a.bytes - b.bytes || a.id.localeCompare(b.id));
}

const VARIANT_CACHE_TTL_MS = 24 * 60 * 60 * 1000;

export async function loadModelVariants(model: ModelEntry): Promise<DiscoveredModelVariant[]> {
  if (!model.repo || !model.filenameStem) return [];
  const key = `quantizationFiles:${model.repo}:${model.filenameStem}`;
  const cache = await chrome.storage.local
    .get<Record<string, { fetchedAt: number; files: ModelFile[] }>>(key)
    .then((items) => items[key])
    .catch(() => undefined);
  const cached = Array.isArray(cache?.files) ? parseModelVariants(model, cache.files) : [];
  const age = Date.now() - (cache?.fetchedAt ?? 0);
  if (cached.length && age >= 0 && age < VARIANT_CACHE_TTL_MS) return cached;

  try {
    const response = await fetch(`https://huggingface.co/api/models/${model.repo}?blobs=true`, {
      signal: AbortSignal.timeout(8000),
    });
    if (!response.ok) throw new Error(`Hugging Face returned HTTP ${response.status}.`);
    const { siblings } = await response.json();
    if (!Array.isArray(siblings)) throw new Error('Invalid Hugging Face file listing.');
    const files = siblings.map((entry) => ({ filename: entry?.rfilename, bytes: entry?.size }));
    const variants = parseModelVariants(model, files);
    if (!variants.length) throw new Error(`No compatible GGML files found for ${model.label}.`);
    const matchedFiles = variants.map(({ filename, bytes }) => ({ filename, bytes }));
    await chrome.storage.local.set({
      [key]: { fetchedAt: Date.now(), files: matchedFiles },
    }).catch(() => {});
    return variants;
  } catch (error) {
    if (cached.length) return cached;
    throw error;
  }
}

export const DEFAULT_MODEL_ID = 'whisper-large-v3';
export const DEFAULT_QUANT_ID = 'q5_0';

const BUILTIN_MODEL_IDS = new Set(BUILTIN_MODELS.map((m) => m.id));

const LEGACY_PRESET_MAP: Record<string, { modelId: string; quantId: string }> = {
  'whisper-large-v3-q5': { modelId: 'whisper-large-v3', quantId: 'q5_0' },
  'whisper-large-v3-f16': { modelId: 'whisper-large-v3', quantId: 'f16' },
  'kotoba-v2.2-f16': { modelId: 'kotoba-v2.2', quantId: 'f16' },
  'kotoba-v2.2-q8': { modelId: 'kotoba-v2.2', quantId: 'q8_0' },
  'kotoba-v2.2-q5': { modelId: 'kotoba-v2.2', quantId: 'q5_0' },
  'kotoba-v2.2-q4': { modelId: 'kotoba-v2.2', quantId: 'q4_k' },
};

export function customModelToEntry(custom: CustomModelDefinition): ModelEntry {
  const filename = custom.filename.trim();
  const isJa =
    custom.japaneseOnly !== undefined
      ? custom.japaneseOnly
      : filename.toLowerCase().includes('kotoba');
  return {
    id: custom.id,
    label: custom.label,
    description: custom.description,
    repo: custom.repo?.trim() || undefined,
    defaultVariantId: 'custom',
    japaneseOnly: isJa,
    supportsDirectS2tt: inferCustomDirectS2tt(filename),
    isCustom: true,
    variants: [{ id: 'custom', label: 'Custom', filename, sizeLabel: 'Custom' }],
  };
}

export function sanitizeCustomModels(raw: unknown): CustomModelDefinition[] {
  if (!Array.isArray(raw)) return [];
  const result: CustomModelDefinition[] = [];
  const seenIds = new Set<string>();
  const seenFiles = new Set<string>();

  for (const item of raw) {
    if (!item || typeof item !== 'object') continue;
    const obj = item as Record<string, unknown>;
    const filename = typeof obj.filename === 'string' ? obj.filename.trim() : '';
    if (!isValidCustomFilename(filename) || seenFiles.has(filename)) continue;

    const rawId = typeof obj.id === 'string' ? obj.id.trim() : '';
    const baseId = rawId || `custom:${filename}`;
    if (BUILTIN_MODEL_IDS.has(baseId) || seenIds.has(baseId)) continue;

    const stem = filename.replace(/\.bin$/i, '');
    const label =
      typeof obj.label === 'string' && obj.label.trim() ? obj.label.trim() : stem;
    const description =
      typeof obj.description === 'string' && obj.description.trim()
        ? obj.description.trim()
        : 'Custom GGML model.';
    const repo = typeof obj.repo === 'string' && obj.repo.trim() ? obj.repo.trim() : undefined;
    const japaneseOnly = typeof obj.japaneseOnly === 'boolean' ? obj.japaneseOnly : undefined;

    seenIds.add(baseId);
    seenFiles.add(filename);
    result.push({
      id: baseId,
      label,
      description,
      repo,
      filename,
      ...(japaneseOnly !== undefined ? { japaneseOnly } : {}),
    });
  }

  return result;
}

export function sanitizeHiddenModelIds(
  raw: unknown,
  customModels: readonly CustomModelDefinition[] = [],
): string[] {
  if (!Array.isArray(raw)) return [];
  const hiddenSet = new Set<string>();
  for (const entry of raw) {
    if (typeof entry === 'string' && BUILTIN_MODEL_IDS.has(entry)) {
      hiddenSet.add(entry);
    }
  }
  if (hiddenSet.size >= BUILTIN_MODELS.length && customModels.length === 0) {
    hiddenSet.delete(DEFAULT_MODEL_ID);
  }
  return Array.from(hiddenSet);
}

export function buildVisibleModels(
  hiddenModelIds: readonly string[] = [],
  customModels: readonly CustomModelDefinition[] = [],
  builtins: readonly ModelEntry[] = BUILTIN_MODELS,
): ModelEntry[] {
  const hidden = new Set(hiddenModelIds);
  const visibleBuiltins = builtins.filter((m) => !hidden.has(m.id));
  const combined = [...visibleBuiltins, ...customModels.map(customModelToEntry)];
  return combined.length > 0 ? combined : [builtins[0] ?? BUILTIN_MODELS[0]!];
}

export function findModelEntry(
  modelId: string,
  customModels: readonly CustomModelDefinition[] = [],
): ModelEntry {
  return buildVisibleModels([], customModels).find((model) => model.id === modelId) ?? BUILTIN_MODELS[0]!;
}

export function pickVariantForModel(
  model: ModelEntry,
  preferredQuantId?: string,
): ModelQuantOption | undefined {
  if (preferredQuantId) {
    const exact = model.variants.find((v) => v.id === preferredQuantId);
    if (exact) return exact;
    if (preferredQuantId.startsWith('q5')) {
      const q5Match = model.variants.find((v) => v.id.startsWith('q5'));
      if (q5Match) return q5Match;
    }
  }
  return model.variants.find((v) => v.id === model.defaultVariantId) ?? model.variants[0];
}

export function resolveModelSource(
  model: ModelEntry,
  quantId?: string,
): NativeModelSource | undefined {
  const variant = pickVariantForModel(model, quantId);
  if (!variant) return undefined;
  return {
    filename: variant.filename,
    repo: model.repo,
    japaneseOnly: model.japaneseOnly,
    supportsDirectS2tt: Boolean(model.supportsDirectS2tt),
  };
}

export function buildFilenameLabelMap(
  customModels: readonly CustomModelDefinition[] = [],
  builtins: readonly ModelEntry[] = BUILTIN_MODELS,
): Map<string, string> {
  const map = new Map<string, string>();
  for (const model of builtins) {
    for (const variant of model.variants) {
      map.set(variant.filename, `${model.label} (${variant.label})`);
    }
  }
  for (const custom of customModels) {
    map.set(custom.filename, custom.label);
  }
  return map;
}

export function migrateLegacyModelSelection(
  savedModelId: string,
  savedQuantId: string | undefined,
  legacyCustomRepo: string,
  legacyCustomFilename: string,
  existingCustomModels: readonly CustomModelDefinition[],
): { modelId: string; quantId: string; customModels: CustomModelDefinition[] } {
  let customModels = [...existingCustomModels];

  const legacy = LEGACY_PRESET_MAP[savedModelId];
  if (Object.hasOwn(LEGACY_PRESET_MAP, savedModelId) && legacy) {
    return {
      modelId: legacy.modelId,
      quantId: legacy.quantId,
      customModels,
    };
  }

  if (savedModelId === 'custom' && isValidCustomFilename(legacyCustomFilename)) {
    const cleanFile = legacyCustomFilename.trim();
    const existing = customModels.find((c) => c.filename === cleanFile);
    if (existing) {
      return { modelId: existing.id, quantId: 'custom', customModels };
    }
    const created: CustomModelDefinition = {
      id: `custom:${cleanFile}`,
      label: cleanFile.replace(/\.bin$/i, ''),
      description: 'Migrated custom GGML model.',
      repo: legacyCustomRepo.trim() || undefined,
      filename: cleanFile,
    };
    customModels = [...customModels, created];
    return { modelId: created.id, quantId: 'custom', customModels };
  }

  const matched = buildVisibleModels([], customModels).find((m) => m.id === savedModelId);
  if (matched) {
    return {
      modelId: matched.id,
      quantId: matched.isCustom ? 'custom' : savedQuantId || matched.defaultVariantId,
      customModels,
    };
  }

  return {
    modelId: DEFAULT_MODEL_ID,
    quantId: DEFAULT_QUANT_ID,
    customModels,
  };
}

export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 ** 2) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 ** 3) return `${(bytes / 1024 ** 2).toFixed(1)} MB`;
  return `${(bytes / 1024 ** 3).toFixed(2)} GB`;
}

export function formatSpeedMBps(bytesPerSec: number): string {
  if (!Number.isFinite(bytesPerSec) || bytesPerSec <= 0) return '0.0 MB/s';
  const mbps = bytesPerSec / (1024 * 1024);
  return `${mbps >= 100 ? mbps.toFixed(0) : mbps.toFixed(1)} MB/s`;
}

export const LANGUAGES: readonly { code: string; label: string }[] = [
  { code: 'ja', label: 'Japanese' },
  { code: 'en', label: 'English' },
  { code: 'id', label: 'Indonesian' },
  { code: 'zh', label: 'Chinese' },
  { code: 'ko', label: 'Korean' },
  { code: 'es', label: 'Spanish' },
  { code: 'fr', label: 'French' },
  { code: 'de', label: 'German' },
  { code: 'auto', label: 'Auto-detect' },
];

export const DEFAULT_LANGUAGE = 'ja';
