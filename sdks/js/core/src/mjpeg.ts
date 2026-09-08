// Motion JPEG: each frame is a complete JPEG image rather than part of a
// coded video sequence.
//
// WebCodecs cannot help here — MJPEG is not in the codec registry, so
// `VideoDecoder` refuses it. Every frame is decoded on its own with
// `createImageBitmap` and wrapped in a `VideoFrame` so the rest of the
// pipeline (presenter, canvas, stats) is unchanged.
//
// This exists because ONVIF Profile S makes MJPEG the one *mandatory* codec
// while H.265 is merely conditional: every conformant camera can emit it, and
// none is obliged to emit anything better. It is a compatibility path and
// costs roughly an order of magnitude more bandwidth than H.264 — never a
// default.

export interface MjpegDecoderOptions {
  onFrame: (frame: VideoFrame) => void;
  onError: (e: Error) => void;
}

export class MjpegDecoder {
  errors = 0;
  decoded = 0;
  /** Always 0: every JPEG stands alone, so nothing is ever waiting on a keyframe. */
  keyframesRequested = 0;

  private closed = false;
  /** Decodes are async; this chain keeps them in capture order. */
  private chain: Promise<void> = Promise.resolve();
  private pending = 0;

  constructor(private o: MjpegDecoderOptions) {}

  static async supported(): Promise<boolean> {
    return typeof createImageBitmap === 'function' && typeof VideoFrame === 'function';
  }

  configure(): void {
    this.closed = false;
  }

  get queueSize(): number { return this.pending; }
  get state(): 'configured' | 'closed' { return this.closed ? 'closed' : 'configured'; }

  /**
   * Every frame is a random access point, so there is nothing to gate on and
   * this never returns false for a missing keyframe.
   */
  decode(data: Uint8Array, _keyframe: boolean, timestampUs: number): boolean {
    if (this.closed) return false;
    // Copy: `data` is a view into a reassembly buffer that is reused, and the
    // decode below reads it after this call returns.
    const bytes = data.slice();
    this.pending++;
    this.chain = this.chain
      .then(async () => {
        if (this.closed) return;
        let bitmap: ImageBitmap;
        try {
          bitmap = await createImageBitmap(new Blob([bytes as BlobPart], { type: 'image/jpeg' }));
        } catch (e) {
          this.errors++;
          this.o.onError(e instanceof Error ? e : new Error(String(e)));
          return;
        }
        if (this.closed) { bitmap.close(); return; }
        // The bitmap's pixels move into the frame; closing the bitmap after is
        // what keeps a 30 fps stream from leaking one image per frame.
        const frame = new VideoFrame(bitmap, { timestamp: timestampUs });
        bitmap.close();
        this.decoded++;
        this.o.onFrame(frame);
      })
      .finally(() => { this.pending--; });
    return true;
  }

  /** No-ops: a JPEG stream has no reference chain to repair. */
  needKeyframe(): void {}
  requestRecovery(): void {}

  close(): void {
    this.closed = true;
  }
}
