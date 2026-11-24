export const DEFAULT_SERVER_URL = 'ws://127.0.0.1:8765';

export interface ModelQuantOption {
  id: string;
  label: string;
  filename: string;
  sizeLabel: string;
}

export interface ModelEntry {
  id: string;
  label: string;
  description: string;
  repo?: string;
  variants: readonly ModelQuantOption[];
  defaultVariantId?: string;
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

// Variants within each model are ordered left = fastest/smallest -> right = best quality.
export const BUILTIN_MODELS: readonly ModelEntry[] = [
  {
    id: 'whisper-large-v3',
    label: 'Whisper Large v3',
    description: 'Multilingual ASR & Direct S2TT to English (best all-round accuracy).',
    repo: 'ggerganov/whisper.cpp',
    defaultVariantId: 'q5_0',
    supportsDirectS2tt: true,
    variants: [
      { id: 'q5_0', label: 'Q5_0', filename: 'ggml-large-v3-q5_0.bin', sizeLabel: '~1.08 GB' },
      { id: 'f16', label: 'FP16', filename: 'ggml-large-v3.bin', sizeLabel: '~3.1 GB' },
    ],
  },
  {
    id: 'kotoba-v2.2',
    label: 'Kotoba Whisper v2.2',
    description: 'Japanese VTubers, anime & casual slang (Japanese-only).',
    repo: 'Pomni/kotoba-whisper-v2.2-ggml-allquants',
    defaultVariantId: 'q8_0',
    japaneseOnly: true,
    supportsDirectS2tt: false,
    variants: [
      { id: 'q4_k', label: 'Q4_K', filename: 'ggml-kotoba-v2.2-q4_k.bin', sizeLabel: '~445 MB' },
      { id: 'q5_0', label: 'Q5_0', filename: 'ggml-kotoba-v2.2-q5_0.bin', sizeLabel: '~540 MB' },
      { id: 'q8_0', label: 'Q8_0', filename: 'ggml-kotoba-v2.2-q8_0.bin', sizeLabel: '~820 MB' },
      { id: 'f16', label: 'FP16', filename: 'ggml-kotoba-v2.2-f16.bin', sizeLabel: '~1.5 GB' },
    ],
  },
  {
    id: 'whisper-large-v3-turbo',
    label: 'Whisper Large v3 Turbo',
    description: 'Fast multilingual transcription (EN, JA, ID, ZH, KO; no Direct S2TT).',
    repo: 'ggerganov/whisper.cpp',
    defaultVariantId: 'q8_0',
    supportsDirectS2tt: false,
    variants: [
      { id: 'q5_0', label: 'Q5_0', filename: 'ggml-large-v3-turbo-q5_0.bin', sizeLabel: '~575 MB' },
      { id: 'q8_0', label: 'Q8_0', filename: 'ggml-large-v3-turbo-q8_0.bin', sizeLabel: '~875 MB' },
      { id: 'f16', label: 'FP16', filename: 'ggml-large-v3-turbo.bin', sizeLabel: '~1.6 GB' },
    ],
  },
  {
    id: 'whisper-medium',
    label: 'Whisper Medium',
    description: 'Mid-size multilingual ASR & lighter Direct S2TT to English.',
    repo: 'ggerganov/whisper.cpp',
    defaultVariantId: 'q8_0',
    supportsDirectS2tt: true,
    variants: [
      { id: 'q5_0', label: 'Q5_0', filename: 'ggml-medium-q5_0.bin', sizeLabel: '~539 MB' },
      { id: 'q8_0', label: 'Q8_0', filename: 'ggml-medium-q8_0.bin', sizeLabel: '~825 MB' },
      { id: 'f16', label: 'FP16', filename: 'ggml-medium.bin', sizeLabel: '~1.5 GB' },
    ],
  },
  {
    id: 'whisper-small',
    label: 'Whisper Small',
    description: 'Fast transcription or light Direct S2TT on low-VRAM GPUs.',
    repo: 'ggerganov/whisper.cpp',
    defaultVariantId: 'q8_0',
    supportsDirectS2tt: true,
    variants: [
      { id: 'q5_1', label: 'Q5_1', filename: 'ggml-small-q5_1.bin', sizeLabel: '~190 MB' },
      { id: 'q8_0', label: 'Q8_0', filename: 'ggml-small-q8_0.bin', sizeLabel: '~265 MB' },
      { id: 'f16', label: 'FP16', filename: 'ggml-small.bin', sizeLabel: '~488 MB' },
    ],
  },
  {
    id: 'whisper-base',
    label: 'Whisper Base',
    description: 'Ultra-fast smoke testing or minimal hardware (supports Direct S2TT).',
    repo: 'ggerganov/whisper.cpp',
    defaultVariantId: 'q8_0',
    supportsDirectS2tt: true,
    variants: [
      { id: 'q5_1', label: 'Q5_1', filename: 'ggml-base-q5_1.bin', sizeLabel: '~60 MB' },
      { id: 'q8_0', label: 'Q8_0', filename: 'ggml-base-q8_0.bin', sizeLabel: '~82 MB' },
      { id: 'f16', label: 'FP16', filename: 'ggml-base.bin', sizeLabel: '~148 MB' },
    ],
  },
];

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
  'whisper-large-v3-turbo-f16': { modelId: 'whisper-large-v3-turbo', quantId: 'f16' },
  'whisper-large-v3-turbo-q8': { modelId: 'whisper-large-v3-turbo', quantId: 'q8_0' },
  'whisper-large-v3-turbo-q5': { modelId: 'whisper-large-v3-turbo', quantId: 'q5_0' },
  'whisper-medium-q8': { modelId: 'whisper-medium', quantId: 'q8_0' },
  'whisper-small-q8': { modelId: 'whisper-small', quantId: 'q8_0' },
  'whisper-base-q8': { modelId: 'whisper-base', quantId: 'q8_0' },
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

export function getAllModels(customModels: readonly CustomModelDefinition[] = []): ModelEntry[] {
  return [...BUILTIN_MODELS, ...customModels.map(customModelToEntry)];
}

export function buildVisibleModels(
  hiddenModelIds: readonly string[] = [],
  customModels: readonly CustomModelDefinition[] = [],
): ModelEntry[] {
  const hidden = new Set(hiddenModelIds);
  const visibleBuiltins = BUILTIN_MODELS.filter((m) => !hidden.has(m.id));
  const combined = [...visibleBuiltins, ...customModels.map(customModelToEntry)];
  return combined.length > 0 ? combined : [BUILTIN_MODELS[0]!];
}

export function findModelEntry(
  modelId: string,
  customModels: readonly CustomModelDefinition[] = [],
): ModelEntry {
  return (
    getAllModels(customModels).find((m) => m.id === modelId) ??
    BUILTIN_MODELS.find((m) => m.id === DEFAULT_MODEL_ID)!
  );
}

export function pickVariantForModel(model: ModelEntry, preferredQuantId?: string): ModelQuantOption {
  if (preferredQuantId) {
    const exact = model.variants.find((v) => v.id === preferredQuantId);
    if (exact) return exact;
    if (preferredQuantId.startsWith('q5')) {
      const q5Match = model.variants.find((v) => v.id.startsWith('q5'));
      if (q5Match) return q5Match;
    }
  }
  if (model.defaultVariantId) {
    const def = model.variants.find((v) => v.id === model.defaultVariantId);
    if (def) return def;
  }
  return model.variants[0]!;
}

export function resolveModelSource(
  modelId: string,
  quantId?: string,
  customModels: readonly CustomModelDefinition[] = [],
): NativeModelSource {
  const model = findModelEntry(modelId, customModels);
  const variant = pickVariantForModel(model, quantId);
  return {
    filename: variant.filename,
    repo: model.repo,
    japaneseOnly: model.japaneseOnly,
    supportsDirectS2tt: Boolean(model.supportsDirectS2tt),
  };
}

export function buildFilenameLabelMap(
  customModels: readonly CustomModelDefinition[] = [],
): Map<string, string> {
  const map = new Map<string, string>();
  for (const model of BUILTIN_MODELS) {
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
  if (legacy) {
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

  const all = getAllModels(customModels);
  const matched = all.find((m) => m.id === savedModelId);
  if (matched) {
    return {
      modelId: matched.id,
      quantId: pickVariantForModel(matched, savedQuantId).id,
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
