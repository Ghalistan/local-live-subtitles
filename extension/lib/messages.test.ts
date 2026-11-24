import { describe, expect, test } from 'bun:test';
import {
  DEFAULT_SERVER_PORT,
  DEFAULT_SUBTITLE_STYLE,
  SPEAKER_COLORS,
  getSpeakerColor,
  hexToRgba,
  parsePortFromServerUrl,
  portToServerUrl,
  pushSubtitleLine,
  sanitizePort,
  sanitizeSubtitleStyle,
} from './messages';

describe('subtitle style helpers', () => {
  test('sanitizes defaults, bounds, and invalid hex colors', () => {
    expect(sanitizeSubtitleStyle()).toEqual(DEFAULT_SUBTITLE_STYLE);
    expect(
      sanitizeSubtitleStyle({
        fontSize: 99,
        bgOpacity: -10,
        textColor: 'red',
        bgColor: '#1a2b3c',
      }),
    ).toEqual({
      fontSize: 56,
      bgOpacity: 0,
      textColor: '#ffffff',
      bgColor: '#1a2b3c',
    });
  });

  test('converts hex color + opacity percentage into rgba()', () => {
    expect(hexToRgba('#000000', 78)).toBe('rgba(0, 0, 0, 0.78)');
    expect(hexToRgba('#3b82f6', 100)).toBe('rgba(59, 130, 246, 1)');
    expect(hexToRgba('#ffffff', 0)).toBe('rgba(255, 255, 255, 0)');
  });

  test('maps speaker IDs deterministically to high-contrast colors', () => {
    expect(getSpeakerColor(1)).toBe(SPEAKER_COLORS[0]);
    expect(getSpeakerColor(2)).toBe(SPEAKER_COLORS[1]);
    expect(getSpeakerColor(9)).toBe(SPEAKER_COLORS[0]);
    expect(getSpeakerColor(0)).toBe(SPEAKER_COLORS[0]);
  });

  test('sanitizes and formats custom server ports safely', () => {
    expect(sanitizePort(1)).toBe(1);
    expect(sanitizePort(8765)).toBe(8765);
    expect(sanitizePort(65535)).toBe(65535);
    expect(sanitizePort(' 9000 ')).toBe(9000);
    expect(sanitizePort(0)).toBeNull();
    expect(sanitizePort(65536)).toBeNull();
    expect(sanitizePort(-5)).toBeNull();
    expect(sanitizePort('8765.5')).toBeNull();
    expect(sanitizePort('abc')).toBeNull();
    expect(sanitizePort('')).toBeNull();

    expect(portToServerUrl(9123)).toBe('ws://127.0.0.1:9123');
    expect(portToServerUrl(0)).toBe(`ws://127.0.0.1:${DEFAULT_SERVER_PORT}`);
    expect(parsePortFromServerUrl('ws://127.0.0.1:9123')).toBe(9123);
    expect(parsePortFromServerUrl('not-a-url')).toBe(DEFAULT_SERVER_PORT);
    expect(parsePortFromServerUrl('ws://127.0.0.1')).toBe(DEFAULT_SERVER_PORT);
  });

  test('manages stacked multi-line interim updates, caps, and expiry cleanly', () => {
    const first = pushSubtitleLine(
      [],
      { text: 'こんにちは', speakers: [1], isFinal: false, updatedAt: 1000 },
      1,
    );
    expect(first.nextId).toBe(2);
    expect(first.lines).toHaveLength(1);
    expect(first.lines[0]?.id).toBe(1);

    // Subsequent interim and final replace trailing interim keeping id=1
    const final1 = pushSubtitleLine(
      first.lines,
      { text: 'こんにちは！', speakers: [1], isFinal: true, updatedAt: 1500 },
      first.nextId,
    );
    expect(final1.nextId).toBe(2);
    expect(final1.lines).toHaveLength(1);
    expect(final1.lines[0]).toMatchObject({ id: 1, text: 'こんにちは！', isFinal: true });

    // Second speaker after final appends a stacked line with new id=2
    const second = pushSubtitleLine(
      final1.lines,
      { text: 'おはよう', speakers: [2], isFinal: true, updatedAt: 2000 },
      final1.nextId,
    );
    expect(second.lines).toHaveLength(2);
    expect(second.lines.map((l) => l.id)).toEqual([1, 2]);

    // Capped at maxLines=3
    const third = pushSubtitleLine(
      second.lines,
      { text: '三行目', speakers: [1, 2], isFinal: true, updatedAt: 2500 },
      second.nextId,
    );
    const fourth = pushSubtitleLine(
      third.lines,
      { text: '四行目', speakers: [1], isFinal: true, updatedAt: 3000 },
      third.nextId,
    );
    expect(fourth.lines.map((l) => l.id)).toEqual([2, 3, 4]);

    // Stale finalized lines older than expireMs=8000 are pruned
    const afterPause = pushSubtitleLine(
      fourth.lines,
      { text: '久しぶり', speakers: [2], isFinal: true, updatedAt: 15000 },
      fourth.nextId,
    );
    expect(afterPause.lines.map((l) => l.text)).toEqual(['久しぶり']);
  });
});
