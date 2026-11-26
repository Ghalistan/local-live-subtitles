import { describe, expect, test } from 'bun:test';
import {
  DEFAULT_TRANSLATOR_TARGET_LANGUAGE,
  getGoogleModelBadge,
  googleModelSupportsDirectS2tt,
  googleModelSupportsTargetLanguage,
  pickDefaultGoogleModel,
  sanitizeGoogleModels,
  sanitizeGoogleTargetLanguage,
  sanitizeTranslatorTargetLanguage,
} from './gemini';

describe('Google Gemini UI model helpers', () => {
  test('sanitizes, sorts, and picks default Google models safely', () => {
    const models = sanitizeGoogleModels([
      { invalid: true },
      { id: '', transport: 'live', kind: 'live' },
      { id: 'bad-transport', transport: 'rpc', kind: 'live' },
      {
        id: 'gemini-2.5-flash',
        displayName: 'Gemini 2.5 Flash',
        supportedGenerationMethods: ['generateContent'],
        transport: 'generate',
        kind: 'multimodal',
      },
      {
        id: 'models/gemini-3.5-transcribe-live',
        displayName: 'Gemini 3.5 Transcribe Live',
        supportedGenerationMethods: ['bidiGenerateContent'],
        transport: 'live',
        kind: 'transcribe-live',
      },
      {
        id: 'gemini-3.5-live-translate-preview',
        displayName: 'Gemini 3.5 Live Translate',
        supportedGenerationMethods: ['bidiGenerateContent'],
        transport: 'live',
        kind: 'translate-live',
      },
    ]);

    expect(models.map((m) => [m.id, m.transport, m.kind])).toEqual([
      ['gemini-3.5-transcribe-live', 'live', 'transcribe-live'],
      ['gemini-3.5-live-translate-preview', 'live', 'translate-live'],
      ['gemini-2.5-flash', 'generate', 'multimodal'],
    ]);

    expect(pickDefaultGoogleModel(models)?.id).toBe('gemini-3.5-transcribe-live');
    expect(pickDefaultGoogleModel(models, 'gemini-2.5-flash')?.id).toBe('gemini-2.5-flash');
    expect(pickDefaultGoogleModel(models, 'missing')?.id).toBe('gemini-3.5-transcribe-live');
    expect(pickDefaultGoogleModel([])).toBeNull();
  });

  test('validates badges, direct S2TT support, and target language sanitization', () => {
    expect(getGoogleModelBadge('transcribe-live')).toBe('Live STT');
    expect(getGoogleModelBadge('translate-live')).toBe('Live Translate');
    expect(getGoogleModelBadge('live')).toBe('Live');
    expect(getGoogleModelBadge('transcribe')).toBe('Transcribe');
    expect(getGoogleModelBadge('multimodal')).toBe('Audio');

    expect(googleModelSupportsDirectS2tt('transcribe-live')).toBe(false);
    expect(googleModelSupportsDirectS2tt('transcribe')).toBe(false);
    expect(googleModelSupportsDirectS2tt('translate-live')).toBe(true);
    expect(googleModelSupportsDirectS2tt('live')).toBe(true);
    expect(googleModelSupportsDirectS2tt('multimodal')).toBe(true);

    expect(googleModelSupportsTargetLanguage('transcribe-live')).toBe(false);
    expect(googleModelSupportsTargetLanguage('translate-live')).toBe(true);

    expect(sanitizeGoogleTargetLanguage('en')).toBe('en');
    expect(sanitizeGoogleTargetLanguage('ja')).toBe('ja');
    expect(sanitizeGoogleTargetLanguage('invalid-lang')).toBe('same');

    expect(sanitizeTranslatorTargetLanguage('ja')).toBe('ja');
    expect(sanitizeTranslatorTargetLanguage('same')).toBe(DEFAULT_TRANSLATOR_TARGET_LANGUAGE);
    expect(sanitizeTranslatorTargetLanguage('invalid')).toBe('en');
  });
});
