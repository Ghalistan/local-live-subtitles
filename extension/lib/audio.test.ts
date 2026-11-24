import { describe, expect, test } from 'bun:test';
import { downsampleTo16k, float32ToPcm16 } from './audio';

describe('downsampleTo16k', () => {
  test('returns input directly when already 16kHz', () => {
    const input = new Float32Array([0.1, -0.2, 0.3]);
    expect(downsampleTo16k(input, 16000)).toBe(input);
  });

  test('downsamples 48kHz to 16kHz at 1/3 length and preserves signal', () => {
    const input = new Float32Array(480);
    for (let i = 0; i < input.length; i++) {
      input[i] = i / input.length;
    }
    const out = downsampleTo16k(input, 48000);
    expect(out.length).toBe(160);
    expect(out[0]).toBeCloseTo(0, 5);
    expect(out[80]).toBeCloseTo(0.5, 2);
  });
});

describe('float32ToPcm16', () => {
  test('converts and clamps float32 audio samples cleanly', () => {
    const input = new Float32Array([-1.5, -1, 0, 0.5, 1, 1.5]);
    const pcm = float32ToPcm16(input);
    expect(Array.from(pcm)).toEqual([-32768, -32768, 0, 16383, 32767, 32767]);
  });
});
