export interface ElevenLabsModelInfo {
  id: string;
  displayName: string;
}

export function sanitizeElevenLabsModels(raw: unknown): ElevenLabsModelInfo[] {
  if (!Array.isArray(raw)) return [];
  const models = new Map<string, ElevenLabsModelInfo>();
  for (const item of raw) {
    if (!item || typeof item !== 'object') continue;
    const { id, displayName } = item as Record<string, unknown>;
    if (typeof id !== 'string' || !/^[a-z0-9][a-z0-9_-]{0,127}$/.test(id) || models.has(id)) {
      continue;
    }
    const label = typeof displayName === 'string' ? displayName.trim() : '';
    models.set(id, { id, displayName: label || id });
  }
  return [...models.values()];
}

export function fetchElevenLabsModelsViaServer(serverUrl: string): Promise<ElevenLabsModelInfo[]> {
  return new Promise((resolve, reject) => {
    const socket = new WebSocket(serverUrl);
    const finish = (result: ElevenLabsModelInfo[] | Error) => {
      clearTimeout(timer);
      socket.onopen = null;
      socket.onmessage = null;
      socket.onerror = null;
      socket.onclose = null;
      socket.close();
      if (result instanceof Error) reject(result);
      else resolve(result);
    };
    const timer = setTimeout(() => {
      finish(new Error('Timed out waiting for the native server to fetch ElevenLabs models.'));
    }, 15000);
    socket.onopen = () => socket.send(JSON.stringify({ type: 'listElevenLabsModels' }));
    socket.onmessage = ({ data }) => {
      try {
        const response = JSON.parse(String(data));
        if (response?.type === 'elevenLabsModels') {
          const models = sanitizeElevenLabsModels(response.models);
          finish(models.length ? models : new Error('No ElevenLabs live speech models were returned.'));
        } else if (response?.type === 'error') {
          finish(new Error(typeof response.error === 'string' ? response.error : 'Could not fetch ElevenLabs models.'));
        }
      } catch {
        // Ignore malformed frames.
      }
    };
    socket.onerror = () => finish(new Error('Could not connect to the native server.'));
    socket.onclose = () => finish(new Error('The native server closed the model list connection.'));
  });
}
