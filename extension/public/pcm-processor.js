const DEFAULT_FRAME_SIZE = 2048;

class PcmProcessor extends AudioWorkletProcessor {
  constructor(options) {
    super();
    const configured = options?.processorOptions?.frameSize;
    this.frameSize = typeof configured === 'number' && configured > 0 ? configured : DEFAULT_FRAME_SIZE;
    this.buffer = new Float32Array(this.frameSize);
    this.offset = 0;
  }

  process(inputs) {
    const channels = inputs[0];
    if (!channels || channels.length === 0) return true;

    const length = channels[0].length;
    const channelCount = channels.length;

    for (let i = 0; i < length; i++) {
      let sample = channels[0][i];
      if (channelCount > 1) {
        for (let c = 1; c < channelCount; c++) {
          sample += channels[c][i];
        }
        sample /= channelCount;
      }

      this.buffer[this.offset++] = sample;
      if (this.offset === this.frameSize) {
        const out = this.buffer;
        this.port.postMessage(out, [out.buffer]);
        this.buffer = new Float32Array(this.frameSize);
        this.offset = 0;
      }
    }

    return true;
  }
}

registerProcessor('pcm-processor', PcmProcessor);
