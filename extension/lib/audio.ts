export const TARGET_SAMPLE_RATE = 16000;

export function downsampleTo16k(input: Float32Array, inputRate: number): Float32Array {
  if (inputRate === TARGET_SAMPLE_RATE || input.length === 0) {
    return input;
  }
  const ratio = inputRate / TARGET_SAMPLE_RATE;
  const outputLength = Math.max(1, Math.round(input.length / ratio));
  const output = new Float32Array(outputLength);
  const maxIndex = input.length - 1;

  for (let i = 0; i < outputLength; i++) {
    const srcPos = i * ratio;
    const idx = Math.floor(srcPos);
    const frac = srcPos - idx;
    const nextIdx = idx < maxIndex ? idx + 1 : maxIndex;
    const s0 = input[idx] ?? 0;
    const s1 = input[nextIdx] ?? s0;
    output[i] = s0 * (1 - frac) + s1 * frac;
  }

  return output;
}

export function float32ToPcm16(samples: Float32Array): Int16Array {
  const pcm = new Int16Array(samples.length);
  for (let i = 0; i < samples.length; i++) {
    const s = samples[i] ?? 0;
    const clamped = s < -1 ? -1 : s > 1 ? 1 : s;
    pcm[i] = clamped < 0 ? clamped * 0x8000 : clamped * 0x7fff;
  }
  return pcm;
}
