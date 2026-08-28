// WebCodecs VideoDecoder wrapper. No jitter buffer: decode() on arrival.
export interface DecoderOptions {
  codec: string;
  onFrame: (frame: VideoFrame) => void;
  onError: (e: Error) => void;
  onNeedKeyframe: () => void;
}

export class Decoder {
  private dec: VideoDecoder | null = null;
  private gotKeyframe = false;
  private lastKeyReqMs = 0;
  errors = 0;
  decoded = 0;
  keyframesRequested = 0;

  constructor(private o: DecoderOptions) {}

  configure(): void {
    this.close();
    this.gotKeyframe = false;
    this.dec = new VideoDecoder({
      output: (frame) => { this.decoded++; this.o.onFrame(frame); },
      error: (e) => { this.errors++; this.o.onError(e instanceof Error ? e : new Error(String(e))); this.needKeyframe(); },
    });
    this.dec.configure({
      codec: this.o.codec,
      optimizeForLatency: true,
      // 'no-preference', not 'prefer-hardware': Chrome treats the latter as a
      // hard requirement and configure() throws where no hardware decoder exists.
      hardwareAcceleration: 'no-preference',
    });
  }

  get queueSize(): number { return this.dec ? this.dec.decodeQueueSize : 0; }
  get state(): CodecState | 'unconfigured' { return this.dec ? this.dec.state : 'unconfigured'; }

  needKeyframe(): void {
    this.gotKeyframe = false;
    const now = performance.now();
    if (now - this.lastKeyReqMs < 250) return;
    this.lastKeyReqMs = now;
    this.keyframesRequested++;
    this.o.onNeedKeyframe();
  }

  /** Returns false if the frame was skipped (no keyframe yet). */
  decode(data: Uint8Array, keyframe: boolean, timestampUs: number): boolean {
    if (!this.dec || this.dec.state !== 'configured') return false;
    if (keyframe) this.gotKeyframe = true;
    else if (!this.gotKeyframe) { this.needKeyframe(); return false; }
    try {
      this.dec.decode(new EncodedVideoChunk({ type: keyframe ? 'key' : 'delta', timestamp: timestampUs, data }));
      return true;
    } catch (e) {
      this.errors++;
      this.needKeyframe();
      return false;
    }
  }

  close(): void {
    if (this.dec) { try { this.dec.close(); } catch { /* already closed */ } }
    this.dec = null;
  }
}
