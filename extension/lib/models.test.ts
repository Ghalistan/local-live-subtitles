import { describe, expect, test } from 'bun:test';
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
  migrateLegacyModelSelection,
  pickVariantForModel,
  resolveModelSource,
  sanitizeCustomModels,
  sanitizeHiddenModelIds,
  sanitizeTargetLanguage,
  type CustomModelDefinition,
} from './models';

describe('model catalog and quantization variants', () => {
  test('orders quantization fastest -> best and configures Large v3 default', () => {
    expect(DEFAULT_MODEL_ID).toBe('whisper-large-v3');
    expect(DEFAULT_QUANT_ID).toBe('q5_0');

    const kotoba = findModelEntry('kotoba-v2.2');
    expect(kotoba.variants.map((v) => v.id)).toEqual(['q4_k', 'q5_0', 'q8_0', 'f16']);

    const large = findModelEntry('whisper-large-v3');
    expect(large.variants.map((v) => v.id)).toEqual(['q5_0', 'f16']);
    expect(large.supportsDirectS2tt).toBe(true);
    expect(kotoba.supportsDirectS2tt).toBe(false);
    expect(findModelEntry('whisper-large-v3-turbo').supportsDirectS2tt).toBe(false);
  });

  test('picks matching quant across models with q5_0 / q5_1 cross-match', () => {
    const kotoba = findModelEntry('kotoba-v2.2');
    const small = findModelEntry('whisper-small');
    const large = findModelEntry('whisper-large-v3');

    expect(pickVariantForModel(small, 'q5_0').id).toBe('q5_1');
    expect(pickVariantForModel(kotoba, 'q5_1').id).toBe('q5_0');
    // Falls back to model's defaultVariantId when preferred quant isn't offered
    expect(pickVariantForModel(large, 'q8_0').id).toBe('q5_0');
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

    const visible = buildVisibleModels(['whisper-base', 'whisper-small'], custom);
    expect(visible.some((m) => m.id === 'whisper-base')).toBe(false);
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

    expect(resolveModelSource('whisper-small', 'q8_0', custom)).toEqual({
      filename: 'ggml-small-q8_0.bin',
      repo: 'ggerganov/whisper.cpp',
      japaneseOnly: undefined,
      supportsDirectS2tt: true,
    });

    expect(resolveModelSource('custom:kotoba-test', 'custom', custom)).toEqual({
      filename: 'my-kotoba-custom.bin',
      repo: undefined,
      japaneseOnly: true,
      supportsDirectS2tt: false,
    });

    const migratedPreset = migrateLegacyModelSelection('kotoba-v2.2-q8', undefined, '', '', []);
    expect(migratedPreset).toEqual({ modelId: 'kotoba-v2.2', quantId: 'q8_0', customModels: [] });

    const migratedCustom = migrateLegacyModelSelection(
      'custom',
      undefined,
      'ggerganov/whisper.cpp',
      'ggml-large-v3-q8_0.bin',
      [],
    );
    expect(migratedCustom.modelId).toBe('custom:ggml-large-v3-q8_0.bin');
    expect(migratedCustom.customModels[0]?.filename).toBe('ggml-large-v3-q8_0.bin');

    const labels = buildFilenameLabelMap(custom);
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
