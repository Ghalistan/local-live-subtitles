import { expect, test } from 'bun:test';
import { fetchElevenLabsModelsViaServer, sanitizeElevenLabsModels } from './elevenlabs';

test('keeps unique safe model IDs and falls back to the ID as the label', () => {
  expect(sanitizeElevenLabsModels([
    { id: 'scribe_v3_live', displayName: ' Scribe v3 Live ' },
    { id: 'scribe_v3_live', displayName: 'duplicate' },
    { id: 'future-model' },
    { id: 'scribe_v3_live&language_code=en' },
    { id: 'a'.repeat(129) },
    { displayName: 'missing ID' },
    null,
  ])).toEqual([
    { id: 'scribe_v3_live', displayName: 'Scribe v3 Live' },
    { id: 'future-model', displayName: 'future-model' },
  ]);
});

test('requests the public model list without credentials and propagates errors', async () => {
  let request: unknown;
  let reply = {
    type: 'elevenLabsModels',
    models: [
      { id: 'scribe_v2_realtime', displayName: 'Scribe v2 Realtime' },
      { id: 'scribe_v3_live', displayName: 'Scribe v3 Live' },
    ],
    error: '',
  };
  const server = Bun.serve({
    hostname: '127.0.0.1',
    port: 0,
    fetch(req, listener) {
      if (listener.upgrade(req)) return;
      return new Response(null, { status: 400 });
    },
    websocket: {
      message(ws, data) {
        request = JSON.parse(String(data));
        ws.send(JSON.stringify(reply));
      },
    },
  });
  const url = `ws://127.0.0.1:${server.port}`;
  try {
    expect(await fetchElevenLabsModelsViaServer(url)).toEqual(reply.models);
    expect(request).toEqual({ type: 'listElevenLabsModels' });

    reply = { type: 'error', models: [], error: 'Model catalog unavailable.' };
    await expect(fetchElevenLabsModelsViaServer(url)).rejects.toThrow('Model catalog unavailable.');

    reply = { type: 'elevenLabsModels', models: [], error: '' };
    await expect(fetchElevenLabsModelsViaServer(url)).rejects.toThrow('No ElevenLabs live speech models');
  } finally {
    await server.stop(true);
  }
});
