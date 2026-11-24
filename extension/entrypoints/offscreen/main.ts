import { downsampleTo16k, float32ToPcm16 } from '../../lib/audio';
import type {
  BackgroundToOffscreenMessage,
  DeviceKind,
  FuriganaToken,
  OffscreenToBackgroundMessage,
} from '../../lib/messages';
import type { NativeModelSource } from '../../lib/models';

interface ServerInbound {
  type: 'progress' | 'ready' | 'activity' | 'transcript' | 'error';
  progress?: number | null;
  label?: string;
  bytesPerSec?: number | null;
  device?: Exclude<DeviceKind, null>;
  activity?: string;
  text?: string;
  tokens?: FuriganaToken[];
  speakers?: number[];
  isFinal?: boolean;
  error?: string;
}

let mediaStream: MediaStream | null = null;
let audioContext: AudioContext | null = null;
let sourceNode: MediaStreamAudioSourceNode | null = null;
let workletNode: AudioWorkletNode | null = null;
let silentGain: GainNode | null = null;

let nativeWs: WebSocket | null = null;
let nativeAudioEl: HTMLAudioElement | null = null;
let nativeReady = false;
let nativeStopping = false;
let isCapturing = false;
let lastActivity = '';

function notifyBackground(msg: OffscreenToBackgroundMessage): void {
  chrome.runtime.sendMessage(msg).catch(() => {});
}

function setActivity(activity: string): void {
  if (!isCapturing || activity === lastActivity) return;
  lastActivity = activity;
  notifyBackground({
    type: 'OFFSCREEN_STATUS',
    status: 'listening',
    activity,
  });
}

function closeNativeSocket(): void {
  nativeReady = false;
  if (!nativeWs) return;
  const ws = nativeWs;
  nativeWs = null;
  nativeStopping = true;
  ws.onopen = null;
  ws.onmessage = null;
  ws.onerror = null;
  ws.onclose = null;
  if (ws.readyState === WebSocket.OPEN) {
    try {
      ws.send(JSON.stringify({ type: 'stop' }));
    } catch {
      // Ignore send failure during shutdown
    }
  }
  if (ws.readyState === WebSocket.OPEN || ws.readyState === WebSocket.CONNECTING) {
    ws.close();
  }
}

async function stopCapture(): Promise<void> {
  isCapturing = false;
  lastActivity = '';
  closeNativeSocket();

  if (nativeAudioEl) {
    nativeAudioEl.pause();
    nativeAudioEl.srcObject = null;
    nativeAudioEl = null;
  }
  if (workletNode) {
    workletNode.port.onmessage = null;
    workletNode.disconnect();
    workletNode = null;
  }
  if (silentGain) {
    silentGain.disconnect();
    silentGain = null;
  }
  if (sourceNode) {
    sourceNode.disconnect();
    sourceNode = null;
  }
  if (audioContext) {
    await audioContext.close().catch(() => {});
    audioContext = null;
  }
  if (mediaStream) {
    for (const track of mediaStream.getTracks()) {
      track.stop();
    }
    mediaStream = null;
  }
}

function connectNativeServer(
  serverUrl: string,
  source: NativeModelSource,
  language: string,
  translate: boolean,
  enableDiarization: boolean,
): Promise<void> {
  closeNativeSocket();
  nativeStopping = false;
  nativeReady = false;

  return new Promise((resolve, reject) => {
    let settled = false;
    const ws = new WebSocket(serverUrl);
    nativeWs = ws;

    const fail = (message: string) => {
      if (!settled) {
        settled = true;
        reject(new Error(message));
      } else {
        notifyBackground({
          type: 'OFFSCREEN_STATUS',
          status: 'error',
          error: message,
        });
      }
      void stopCapture();
    };

    ws.onopen = () => {
      ws.send(
        JSON.stringify({
          type: 'start',
          filename: source.filename,
          repo: source.repo,
          url: source.url,
          language,
          japaneseOnly: source.japaneseOnly,
          translate,
          diarization: enableDiarization,
        }),
      );
    };

    ws.onmessage = (event) => {
      if (typeof event.data !== 'string') return;
      let msg: ServerInbound;
      try {
        msg = JSON.parse(event.data) as ServerInbound;
      } catch {
        return;
      }

      if (msg.type === 'progress') {
        notifyBackground({
          type: 'OFFSCREEN_STATUS',
          status: 'loading',
          progress: msg.progress ?? null,
          progressLabel: msg.label ?? 'Preparing model...',
          speedBps: msg.bytesPerSec ?? null,
        });
      } else if (msg.type === 'ready') {
        nativeReady = true;
        lastActivity = isCapturing ? 'Waiting for speech...' : '';
        notifyBackground({
          type: 'OFFSCREEN_STATUS',
          status: isCapturing ? 'listening' : 'idle',
          device: msg.device ?? 'vulkan',
          activity: lastActivity,
          progress: isCapturing ? 100 : null,
          progressLabel: '',
          speedBps: null,
          error: null,
        });
        if (!settled) {
          settled = true;
          resolve();
        }
      } else if (msg.type === 'activity' && msg.activity) {
        setActivity(msg.activity);
      } else if (msg.type === 'transcript' && msg.text) {
        notifyBackground({
          type: 'OFFSCREEN_TRANSCRIPT',
          text: msg.text,
          tokens: msg.tokens,
          speakers: msg.speakers,
          isFinal: Boolean(msg.isFinal),
        });
      } else if (msg.type === 'error') {
        fail(msg.error || 'Native ASR server error');
      }
    };

    ws.onerror = () => {
      if (!nativeStopping) {
        fail(
          `Could not connect to native server at ${serverUrl}. Run: cd native && cargo run --release`,
        );
      }
    };

    ws.onclose = () => {
      if (!nativeStopping) {
        fail(`Native server connection closed (${serverUrl}).`);
      }
    };
  });
}

async function startCapture(
  streamId: string,
  source: NativeModelSource,
  language: string,
  translate: boolean,
  enableDiarization: boolean,
  serverUrl: string,
): Promise<void> {
  await stopCapture();
  isCapturing = true;

  const constraints = {
    audio: {
      mandatory: {
        chromeMediaSource: 'tab',
        chromeMediaSourceId: streamId,
      },
    },
    video: false,
  } as unknown as MediaStreamConstraints;
  mediaStream = await navigator.mediaDevices.getUserMedia(constraints);

  // Preserve untouched stereo speaker output via native <audio> while capturing at 16kHz
  nativeAudioEl = new Audio();
  nativeAudioEl.srcObject = mediaStream;
  await nativeAudioEl.play().catch((err) => {
    console.warn('[subtitles] Native playback fallback warning:', err);
  });

  // Capture at 16kHz directly so Chromium handles anti-aliased resampling in C++
  audioContext = new AudioContext({ sampleRate: 16000 });
  if (audioContext.state === 'suspended') {
    await audioContext.resume();
  }
  await audioContext.audioWorklet.addModule(chrome.runtime.getURL('/pcm-processor.js'));

  sourceNode = audioContext.createMediaStreamSource(mediaStream);
  workletNode = new AudioWorkletNode(audioContext, 'pcm-processor', {
    processorOptions: { frameSize: 512 },
  });
  silentGain = audioContext.createGain();
  silentGain.gain.value = 0;

  // Keep worklet graph pulled by destination at zero gain so Chromium never culls process()
  sourceNode.connect(workletNode);
  workletNode.connect(silentGain);
  silentGain.connect(audioContext.destination);

  workletNode.port.onmessage = (event: MessageEvent<Float32Array>) => {
    if (!nativeReady || !nativeWs || nativeWs.readyState !== WebSocket.OPEN || !audioContext) return;
    if (nativeWs.bufferedAmount > 65536) return;
    const mono16k =
      audioContext.sampleRate === 16000
        ? event.data
        : downsampleTo16k(event.data, audioContext.sampleRate);
    const pcm = float32ToPcm16(mono16k);
    nativeWs.send(pcm as BufferSource);
  };

  await connectNativeServer(serverUrl, source, language, translate, enableDiarization);
}

chrome.runtime.onMessage.addListener((message: BackgroundToOffscreenMessage) => {
  if (message.type === 'OFFSCREEN_START') {
    startCapture(
      message.streamId,
      message.source,
      message.language,
      message.translate,
      message.enableDiarization,
      message.serverUrl,
    ).catch((err) => {
      isCapturing = false;
      notifyBackground({
        type: 'OFFSCREEN_STATUS',
        status: 'error',
        error: err instanceof Error ? err.message : String(err),
      });
    });
  } else if (message.type === 'OFFSCREEN_SET_DIARIZATION') {
    if (nativeWs && nativeWs.readyState === WebSocket.OPEN) {
      nativeWs.send(
        JSON.stringify({
          type: 'setDiarization',
          enabled: message.enabled,
        }),
      );
    }
  } else if (message.type === 'OFFSCREEN_STOP') {
    void stopCapture();
  }
});
