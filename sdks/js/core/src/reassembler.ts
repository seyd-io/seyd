// Per-channel frame reassembly with per-block Reed-Solomon recovery.
// Rules are docs/protocol/chunks.md "Receiver rules".
import { decode as fecDecode } from './fec.js';
import { ChunkHeader, FLAG2_END_OF_FRAME, FLAG2_FRAME_META, parseFrameMeta, seqDelta } from './wire.js';

export const MAX_REORDER = 4;

export interface AssembledFrame {
  frameId: number;
  keyframe: boolean;
  data: Uint8Array;
  captureTsUs: bigint | null;
  seqInGop: number | null;
  sendTsFirst: number;
  sendTsLast: number;
  firstSeenMs: number;
  lastSeenMs: number;
  recovered: boolean;
}

export interface LossEvent { frameId: number; keyframe: boolean; superseded: boolean; chunksMissing: number }

export interface ReassemblerCounters {
  chunksDup: number; chunksTooOld: number; chunksTooLate: number; chunksMissing: number;
  framesSeen: number; framesClean: number; framesRecovered: number; framesIncomplete: number; framesTooLate: number;
  keyframesClean: number; keyframesLost: number;
  /** Frames closed by the silence timer (as opposed to superseded by a newer decodable frame). */
  framesTimedOut: number;
  /** Current adaptive silence deadline for delta frames (ms). */
  deadlineDeltaEffectiveMs: number;
}

interface Block {
  n: number; k: number; chunkLen: number; lastLen: number;
  data: (Uint8Array | null)[]; parity: (Uint8Array | null)[];
  dataRx: number; parityRx: number; hasMeta: boolean;
  done: Uint8Array | null; recovered: boolean;
}

interface FrameState {
  id: number; keyframe: boolean;
  blocks: Map<number, Block>;
  lastBlockIdx: number | null;
  firstSeen: number; lastSeen: number; sendTsFirst: number; sendTsLast: number;
  timer: ReturnType<typeof setTimeout> | null;
  closed: boolean; recovered: boolean;
  /** Largest gap between consecutive chunks of this frame (ms) — the jitter signal. */
  maxGapMs: number;
}

export interface ReassemblerOptions {
  deadlineDeltaMs: number;
  deadlineKeyMs: number;
  onFrame: (f: AssembledFrame) => void;
  onLoss: (l: LossEvent) => void;
  now?: () => number;
}

export class Reassembler {
  readonly counters: ReassemblerCounters = {
    chunksDup: 0, chunksTooOld: 0, chunksTooLate: 0, chunksMissing: 0,
    framesSeen: 0, framesClean: 0, framesRecovered: 0, framesIncomplete: 0, framesTooLate: 0,
    keyframesClean: 0, keyframesLost: 0, framesTimedOut: 0, deadlineDeltaEffectiveMs: 0,
  };
  private frames = new Map<number, FrameState>();
  private newestId: number | null = null;
  private lastDecodedId: number | null = null;
  /** Ring of per-frame max chunk gaps (ms) over recent frames; p95 drives the adaptive deadline. */
  private gaps: number[] = [];
  deadlineDeltaMs: number;
  deadlineKeyMs: number;
  private now: () => number;

  constructor(private o: ReassemblerOptions) {
    this.deadlineDeltaMs = o.deadlineDeltaMs;
    this.deadlineKeyMs = o.deadlineKeyMs;
    this.now = o.now ?? (() => performance.now());
  }

  /** Forget everything (e.g. after a decoder reset). */
  reset(): void {
    for (const f of this.frames.values()) if (f.timer) clearTimeout(f.timer);
    this.frames.clear();
    this.newestId = null;
    this.lastDecodedId = null;
  }

  push(h: ChunkHeader, payload: Uint8Array): void {
    if (this.newestId === null || seqDelta(h.frameId, this.newestId) > 0) this.newestId = h.frameId;
    if (seqDelta(this.newestId, h.frameId) > MAX_REORDER) { this.counters.chunksTooOld++; return; }
    if (this.lastDecodedId !== null && seqDelta(h.frameId, this.lastDecodedId) <= 0) { this.counters.chunksTooLate++; return; }

    const t = this.now();
    let f = this.frames.get(h.frameId);
    if (!f) {
      f = { id: h.frameId, keyframe: h.keyframe, blocks: new Map(), lastBlockIdx: null,
            firstSeen: t, lastSeen: t, sendTsFirst: h.sendTs, sendTsLast: h.sendTs,
            timer: null, closed: false, recovered: false, maxGapMs: 0 };
      this.frames.set(h.frameId, f);
      this.counters.framesSeen++;
    }
    if (f.closed) return;
    f.maxGapMs = Math.max(f.maxGapMs, t - f.lastSeen);
    f.lastSeen = t;
    f.sendTsLast = h.sendTs;
    if (h.keyframe) f.keyframe = true;
    if (h.flags2 & FLAG2_END_OF_FRAME) f.lastBlockIdx = h.blockIdx;

    // Close-out measures silence, not elapsed time: re-arm on every chunk.
    // The profile's deadline is a LAN number; on a jittery path a frame's
    // chunks legitimately arrive spread out, and closing it out early turns
    // jitter into fake loss and keyframe requests (measured on cellular: 33
    // "lost" frames at 0.0 % true loss). The deadline therefore adapts to the
    // observed intra-frame gap (p95 of recent frames), bounded at 250 ms. This
    // adds no latency in the normal case: a frame is also closed the moment a
    // newer frame decodes, which is the real bound.
    if (f.timer) clearTimeout(f.timer);
    const ref = f;
    f.timer = setTimeout(() => this.closeFrame(ref, false), this.effectiveDeadline(f.keyframe));

    let b = f.blocks.get(h.blockIdx);
    if (!b) {
      b = { n: h.n, k: h.k, chunkLen: h.chunkLen, lastLen: h.lastLen,
            data: new Array(h.n).fill(null), parity: new Array(h.k).fill(null),
            dataRx: 0, parityRx: 0, hasMeta: false, done: null, recovered: false };
      f.blocks.set(h.blockIdx, b);
    }
    if (b.done) { this.counters.chunksDup++; return; }
    const isParity = h.chunkIdx >= h.n;
    const slot = isParity ? h.chunkIdx - h.n : h.chunkIdx;
    const target = isParity ? b.parity : b.data;
    if (slot >= target.length || target[slot]) { this.counters.chunksDup++; return; }
    if (h.chunkIdx === 0 && h.blockIdx === 0 && (h.flags2 & FLAG2_FRAME_META)) b.hasMeta = true;

    // Parity was computed over zero-padded chunks, so the short final data
    // chunk is re-padded before it can take part in recovery. Always copy —
    // the transport buffer is reused.
    let body: Uint8Array;
    if (payload.byteLength < b.chunkLen) { body = new Uint8Array(b.chunkLen); body.set(payload); }
    else body = payload.slice(0, b.chunkLen);
    target[slot] = body;
    if (isParity) b.parityRx++; else b.dataRx++;

    if (b.dataRx === b.n) this.finishBlock(b, false);
    else if (b.dataRx + b.parityRx >= b.n && b.k > 0) {
      const rec = fecDecode(b.data, b.parity);
      if (rec) { b.data = rec; b.dataRx = b.n; this.finishBlock(b, true); }
    }
    if (b.done) this.maybeFinishFrame(f);
  }

  private finishBlock(b: Block, viaFec: boolean): void {
    const total = (b.n - 1) * b.chunkLen + b.lastLen;
    const out = new Uint8Array(total);
    let off = 0;
    for (let i = 0; i < b.n; i++) {
      const take = i === b.n - 1 ? b.lastLen : b.chunkLen;
      out.set(b.data[i]!.subarray(0, take), off);
      off += take;
    }
    b.done = out;
    b.recovered = viaFec;
    b.data = [];
    b.parity = [];
  }

  private maybeFinishFrame(f: FrameState): void {
    if (f.lastBlockIdx === null) return;
    let total = 0;
    for (let i = 0; i <= f.lastBlockIdx; i++) {
      const b = f.blocks.get(i);
      if (!b || !b.done) return;
      total += b.done.length;
      if (b.recovered) f.recovered = true;
    }
    const buf = new Uint8Array(total);
    let off = 0;
    for (let i = 0; i <= f.lastBlockIdx; i++) { const d = f.blocks.get(i)!.done!; buf.set(d, off); off += d.length; }

    f.closed = true;
    if (f.timer) clearTimeout(f.timer);
    this.frames.delete(f.id);
    this.noteGap(f);
    if (f.recovered) this.counters.framesRecovered++; else this.counters.framesClean++;
    if (f.keyframe) this.counters.keyframesClean++;
    this.lastDecodedId = f.id;
    // Anything older can never be decoded now.
    for (const [id, other] of this.frames) if (seqDelta(id, f.id) < 0) this.closeFrame(other, true);

    let data: Uint8Array = buf;
    let captureTsUs: bigint | null = null;
    let seqInGop: number | null = null;
    if (f.blocks.get(0)!.hasMeta) {
      const m = parseFrameMeta(buf);
      if (m) { captureTsUs = m.meta.captureTsUs; seqInGop = m.meta.seqInGop; data = m.rest; }
    }
    this.o.onFrame({
      frameId: f.id, keyframe: f.keyframe, data, captureTsUs, seqInGop,
      sendTsFirst: f.sendTsFirst, sendTsLast: f.sendTsLast,
      firstSeenMs: f.firstSeen, lastSeenMs: f.lastSeen, recovered: f.recovered,
    });
  }

  private effectiveDeadline(keyframe: boolean): number {
    const base = keyframe ? this.deadlineKeyMs : this.deadlineDeltaMs;
    if (this.gaps.length < 10) return base;
    // p99 of recent max intra-frame gaps, ×3 + 20 ms: cellular schedulers
    // deliver in bursts, and a timeout costs a keyframe round trip while a
    // longer wait costs nothing when the next frame arrives whole.
    const sorted = [...this.gaps].sort((a, b) => a - b);
    const p99 = sorted[Math.min(sorted.length - 1, Math.floor(sorted.length * 0.99))];
    const adaptive = Math.min(250, Math.max(base, Math.ceil(p99 * 3 + 20)));
    if (!keyframe) this.counters.deadlineDeltaEffectiveMs = adaptive;
    return keyframe ? Math.max(adaptive, base) : adaptive;
  }

  private noteGap(f: FrameState): void {
    this.gaps.push(f.maxGapMs);
    if (this.gaps.length > 200) this.gaps.shift();
  }

  private closeFrame(f: FrameState, superseded: boolean): void {
    if (f.closed) return;
    f.closed = true;
    if (f.timer) clearTimeout(f.timer);
    this.frames.delete(f.id);
    let missing = 0;
    for (const b of f.blocks.values()) if (!b.done) missing += b.n - b.dataRx;
    this.counters.chunksMissing += missing;
    this.counters.framesIncomplete++;
    if (!superseded) this.counters.framesTimedOut++;
    if (f.keyframe) this.counters.keyframesLost++;
    this.o.onLoss({ frameId: f.id, keyframe: f.keyframe, superseded, chunksMissing: missing });
  }
}
