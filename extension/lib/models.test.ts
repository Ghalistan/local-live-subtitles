import { describe, expect, spyOn, test } from 'bun:test';
import {
  BUILTIN_MODELS,
  DEFAULT_MODEL_ID,
  DEFAULT_QUANT_ID,
  buildFilenameLabelMap,
  buildVisibleModels,
  findModelEntry,
  formatBytes,
  formatSpeedMBps,
  inferCustomDirectS2tt,
  loadModelVariants,
  migrateLegacyModelSelection,
  parseModelVariants,
  pickVariantForModel,
  resolveModelSource,
  sanitizeCustomModels,
  sanitizeHiddenModelIds,
  sanitizeTargetLanguage,
  type CustomModelDefinition,
} from './models';

describe('model catalog and quantization variants', () => {
  const large = findModelEntry('whisper-large-v3');
  const kotoba = findModelEntry('kotoba-v2.2');
  const files = [
    { filename: 'ggml-large-v3.bin', bytes: 8192 },
    { filename: 'ggml-large-v3-q5_0.bin', bytes: 4096 },
  ];
  const hydratedLarge = { ...large, variants: parseModelVariants(large, files) };

  test('offers exactly two families and waits for a discovered filename', () => {
    expect(DEFAULT_MODEL_ID).toBe('whisper-large-v3');
    expect(DEFAULT_QUANT_ID).toBe('q5_0');
    expect(BUILTIN_MODELS.map((model) => model.id)).toEqual(['whisper-large-v3', 'kotoba-v2.2']);
    expect(resolveModelSource(large, DEFAULT_QUANT_ID)).toBeUndefined();
    expect(large.supportsDirectS2tt).toBe(true);
    expect(kotoba.japaneseOnly).toBe(true);
    expect(kotoba.supportsDirectS2tt).toBe(false);
  });

  test('filters local and remote files by family, deduplicates, and sorts by size', () => {
    const variants = parseModelVariants(large, [
      ...files,
      { filename: 'ggml-large-v3-turbo-q5_0.bin', bytes: 100 },
      { filename: 'subdir/ggml-large-v3-q8_0.bin', bytes: 100 },
      { filename: 'ggml-large-v3-q8_0.bin', bytes: 0 },
      { filename: 'ggml-large-v3-f16.bin', bytes: 9999 },
    ]);
    expect(variants).toEqual([
      { id: 'q5_0', label: 'Q5_0', filename: files[1]!.filename, bytes: 4096, sizeLabel: '4.0 KB' },
      { id: 'f16', label: 'FP16', filename: files[0]!.filename, bytes: 8192, sizeLabel: '8.0 KB' },
    ]);
    expect(parseModelVariants(kotoba, [
      ...files,
      { filename: 'ggml-kotoba-v2.2-f32.bin', bytes: 2048 },
      { filename: 'ggml-kotoba-v2.2-q6_k.bin', bytes: 512 },
    ]).map((variant) => variant.label)).toEqual(['Q6_K', 'FP32']);
    expect(pickVariantForModel(hydratedLarge, 'q5_1')?.id).toBe('q5_0');
    expect(pickVariantForModel(hydratedLarge, 'q8_0')?.id).toBe('q5_0');
  });

  test('caches listings per family, refreshes after a day, and reuses stale data on error', async () => {
    const stored: Record<string, unknown> = {};
    const previousChrome = Object.getOwnPropertyDescriptor(globalThis, 'chrome');
    const local = {
      get: async (key: string) => ({ [key]: stored[key] }),
      set: async (entries: Record<string, unknown>) => Object.assign(stored, entries),
    };
    Object.defineProperty(globalThis, 'chrome', { configurable: true, value: { storage: { local } } });
    const fetchListing = spyOn(globalThis, 'fetch').mockRejectedValue(new Error('unexpected request'));
    const now = Date.now();
    const clock = spyOn(Date, 'now').mockReturnValue(now);
    try {
      fetchListing.mockResolvedValueOnce(Response.json({
        siblings: files.map(({ filename, bytes }) => ({ rfilename: filename, size: bytes })),
      }));
      expect(await loadModelVariants(large)).toEqual(hydratedLarge.variants);
      expect(await loadModelVariants(large)).toEqual(hydratedLarge.variants);
      expect(fetchListing).toHaveBeenCalledTimes(1);
      expect(fetchListing.mock.calls[0]?.[0]).toBe(`https://huggingface.co/api/models/${large.repo}?blobs=true`);

      clock.mockReturnValue(now + 24 * 60 * 60 * 1000);
      fetchListing.mockResolvedValueOnce(Response.json({
        siblings: [{ rfilename: 'ggml-large-v3-q8_0.bin', size: 6144 }],
      }));
      const refreshed = await loadModelVariants(large);
      expect(refreshed.map((variant) => variant.id)).toEqual(['q8_0']);
      expect(fetchListing).toHaveBeenCalledTimes(2);

      clock.mockReturnValue(now + 48 * 60 * 60 * 1000);
      fetchListing.mockRejectedValueOnce(new Error('offline'));
      expect(await loadModelVariants(large)).toEqual(refreshed);
      fetchListing.mockResolvedValueOnce(new Response(null, { status: 503 }));
      await expect(loadModelVariants(kotoba)).rejects.toThrow('503');
      expect(fetchListing).toHaveBeenCalledTimes(4);
    } finally {
      clock.mockRestore();
      fetchListing.mockRestore();
      if (previousChrome) Object.defineProperty(globalThis, 'chrome', previousChrome);
      else Reflect.deleteProperty(globalThis, 'chrome');
    }
  });

  test('hides built-in models safely and appends valid custom models', () => {
    const allIds = BUILTIN_MODELS.map((m) => m.id);
    const clampedHidden = sanitizeHiddenModelIds(allIds, []);
    expect(clampedHidden).not.toContain(DEFAULT_MODEL_ID);
    expect(buildVisibleModels(clampedHidden, []).length).toBe(1);

    const custom = sanitizeCustomModels([
      {
        id: 'custom:my-ja',
        label: 'My Custom',
        description: '',
        repo: ' ggerganov/whisper.cpp ',
        filename: ' ggml-large-v3-q8_0.bin ',
      },
      { id: 'invalid', filename: '../evil.onnx' },
      { id: 'dup', filename: 'ggml-large-v3-q8_0.bin' },
    ]);

    expect(custom).toEqual([
      {
        id: 'custom:my-ja',
        label: 'My Custom',
        description: 'Custom GGML model.',
        repo: 'ggerganov/whisper.cpp',
        filename: 'ggml-large-v3-q8_0.bin',
      },
    ]);

    const visible = buildVisibleModels([large.id], custom);
    expect(visible.some((m) => m.id === large.id)).toBe(false);
    expect(visible.at(-1)?.isCustom).toBe(true);
  });

  test('resolves built-in and custom sources and migrates legacy selections', () => {
    const custom: CustomModelDefinition[] = [
      {
        id: 'custom:kotoba-test',
        label: 'Custom Kotoba',
        description: 'Japanese test build',
        filename: 'my-kotoba-custom.bin',
      },
    ];

    expect(resolveModelSource(hydratedLarge, 'q5_0')).toEqual({
      filename: 'ggml-large-v3-q5_0.bin',
      repo: 'ggerganov/whisper.cpp',
      japaneseOnly: undefined,
      supportsDirectS2tt: true,
    });

    expect(resolveModelSource(findModelEntry('custom:kotoba-test', custom), 'custom')).toEqual({
      filename: 'my-kotoba-custom.bin',
      repo: undefined,
      japaneseOnly: true,
      supportsDirectS2tt: false,
    });

    const migratedPreset = migrateLegacyModelSelection('kotoba-v2.2-q8', undefined, '', '', []);
    expect(migratedPreset).toEqual({ modelId: 'kotoba-v2.2', quantId: 'q8_0', customModels: [] });
    expect(migrateLegacyModelSelection('kotoba-v2.2', 'q6_k', '', '', custom))
      .toEqual({ modelId: 'kotoba-v2.2', quantId: 'q6_k', customModels: custom });
    expect(migrateLegacyModelSelection('whisper-small-q8', undefined, '', '', custom))
      .toEqual({ modelId: DEFAULT_MODEL_ID, quantId: DEFAULT_QUANT_ID, customModels: custom });

    const migratedCustom = migrateLegacyModelSelection(
      'custom',
      undefined,
      'ggerganov/whisper.cpp',
      'ggml-large-v3-q8_0.bin',
      [],
    );
    expect(migratedCustom.modelId).toBe('custom:ggml-large-v3-q8_0.bin');
    expect(migratedCustom.customModels[0]?.filename).toBe('ggml-large-v3-q8_0.bin');

    const labels = buildFilenameLabelMap(custom, [hydratedLarge]);
    expect(labels.get('ggml-large-v3-q5_0.bin')).toBe('Whisper Large v3 (Q5_0)');
    expect(labels.get('my-kotoba-custom.bin')).toBe('Custom Kotoba');
  });

  test('validates direct s2tt heuristics, target language, and byte formatting', () => {
    expect(inferCustomDirectS2tt('ggml-large-v3-q5_0.bin')).toBe(true);
    expect(inferCustomDirectS2tt('ggml-large-v3-turbo-q8_0.bin')).toBe(false);
    expect(inferCustomDirectS2tt('ggml-kotoba-v2.2-q8_0.bin')).toBe(false);
    expect(inferCustomDirectS2tt('distil-large-v3.bin')).toBe(false);
    expect(inferCustomDirectS2tt('ggml-small.en.bin')).toBe(false);

    expect(sanitizeTargetLanguage('en')).toBe('en');
    expect(sanitizeTargetLanguage('ja')).toBe('same');

    expect(formatBytes(512)).toBe('512 B');
    expect(formatBytes(1024 * 15)).toBe('15.0 KB');
    expect(formatBytes(1024 * 1024 * 820)).toBe('820.0 MB');
    expect(formatBytes(1024 ** 3 * 1.5)).toBe('1.50 GB');

    expect(formatSpeedMBps(0)).toBe('0.0 MB/s');
    expect(formatSpeedMBps(512 * 1024)).toBe('0.5 MB/s');
    expect(formatSpeedMBps(12.5 * 1024 * 1024)).toBe('12.5 MB/s');
    expect(formatSpeedMBps(120 * 1024 * 1024)).toBe('120 MB/s');
  });
});
