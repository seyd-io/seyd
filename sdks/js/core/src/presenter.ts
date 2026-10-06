// Copyright 2026 Anton Gravestam
// SPDX-License-Identifier: Apache-2.0
// Presentation pacing: decoded frames are shown on the timeline the wire
// carries (`capture_ts_us`), not at the instant their bytes happened to arrive.
//
// Decoding on arrival puts every millisecond of sender and network jitter onto
// the glass. The worst offender is structural rather than a fault: a keyframe
// is ~8x the payload of a delta frame and is produced in one frame slot, so
// once per GOP the picture holds for two or three frame times and then advances
// twice within a millisecond. On a continuous pan that reads as a periodic jerk
// at exactly the keyframe rate.
//
// A frame is therefore held until its capture timestamp is due, offset by a
// fixed delay that buys the pipeline room to be late. That delay is the entire
// cost of this class — it is added to glass-to-glass latency — so `delayMs: 0`
// restores decode-on-arrival exactly, byte for byte.
//
// The capture clock and the pilot clock share no epoch, so the mapping between
// them is learned rather than assumed: `lag = arrival − capture` is constant
// apart from pipeline delay, and its minimum over a few seconds is the fastest
// transit recently observed. Scheduling against that minimum means a frame that
// takes the fastest path waits the full delay, and one that is late waits
// correspondingly less — self-calibrating, with no clock sync needed. The
// minimum is slewed rather than jumped so the schedule glides when a fast
// sample ages out of the window.
export interface PresenterOptions {
  /** Presentation delay in ms. 0 disables pacing: draw as soon as decoded. */
  delayMs: number;
  /** Paint one frame. Takes ownership: `draw` must `close()` (or transfer) it. */
  draw: (frame: VideoFrame) => void;
  now?: () => number;
}

export interface PresenterCounters {
  shown: number;
  /** Frames superseded by a newer already-due frame (catch-up after a stall). */
  skippedLate: number;
  /** Times the capture timeline jumped and the mapping was rebuilt. */
  reanchors: number;
  /** Frames waiting for their slot right now. */
  queued: number;
  /** Smoothed |actual − scheduled| paint error (ms): the judder left over. */
  jitterMs: number;
}

/** How far back the fastest-transit estimate looks. */
const LAG_WINDOW_MS = 4000;
/** A lag step this large is a new timeline (robot restart), not a late frame. */
const RESET_MS = 1000;
/** Ceiling on how fast the schedule may glide, ms per frame. */
const SLEW_MS = 2;
/**
 * When a burst arrives late, frames are drained at this fraction of their
 * capture spacing — 11 % faster than real time. Painting a backlog as fast as
 * the timers fire is the burst the operator sees as a jump; refusing to catch
 * up at all would hold the added latency forever.
 */
const CATCHUP = 0.9;
/** A backlog this deep means we are not keeping up at all; collapse it. */
const MAX_QUEUE = 32;

export class Presenter {
  readonly counters: PresenterCounters = { shown: 0, skippedLate: 0, reanchors: 0, queued: 0, jitterMs: 0 };
  /** Decoded frames waiting for their slot, with the moment each arrived. */
  private q: { f: VideoFrame; at: number }[] = [];
  private timer: ReturnType<typeof setTimeout> | null = null;
  private lags: { t: number; lag: number }[] = [];
  private minLag = 0;
  private have = false;
  /** Local time and scheduled slot of the frame painted last, for catch-up spacing. */
  private lastPaintAt: number | null = null;
  private lastPaintDue = 0;
  private now: () => number;

  constructor(private o: PresenterOptions) {
    this.now = o.now ?? (() => performance.now());
  }

  get delayMs(): number { return this.o.delayMs; }
  setDelay(ms: number): void {
    this.o.delayMs = Math.max(0, ms);
    if (this.o.delayMs === 0) this.flush(Infinity);
    else this.schedule();
  }

  push(frame: VideoFrame): void {
    // Unpaced: the old behaviour, kept exact so the delay can be A/B'd.
    if (this.o.delayMs <= 0) { this.paint(frame); return; }

    const now = this.now();
    const lag = now - frame.timestamp / 1000;
    if (!this.have || Math.abs(lag - this.lags[this.lags.length - 1].lag) > RESET_MS) {
      if (this.have) this.counters.reanchors++;
      this.lags = [];
      this.minLag = lag;
      this.have = true;
    }
    this.lags.push({ t: now, lag });
    while (this.lags.length > 1 && this.lags[0].t < now - LAG_WINDOW_MS) this.lags.shift();
    let target = Infinity;
    for (const s of this.lags) if (s.lag < target) target = s.lag;
    this.minLag += Math.max(-SLEW_MS, Math.min(SLEW_MS, target - this.minLag));

    this.q.push({ f: frame, at: now });
    if (this.q.length > MAX_QUEUE) {
      while (this.q.length > 1) { this.counters.skippedLate++; this.q.shift()!.f.close(); }
    }
    this.counters.queued = this.q.length;
    this.schedule();
  }

  /** Drop everything pending (transport teardown); frames are closed, not shown. */
  reset(): void {
    if (this.timer !== null) { clearTimeout(this.timer); this.timer = null; }
    for (const e of this.q) e.f.close();
    this.q = [];
    this.lags = [];
    this.have = false;
    this.lastPaintAt = null;
    this.counters.queued = 0;
  }

  private dueMs(captureUs: number): number {
    return captureUs / 1000 + this.minLag + this.o.delayMs;
  }

  /**
   * When the head frame should be painted: its own slot, never sooner than a
   * catch-up-limited step after the frame before it — and never later than the
   * configured delay past its own arrival. That last clause is what bounds the
   * cost: a source whose sampling clock runs slower than ours (the demo camera
   * is ~1.6 % slow) walks the capture timeline steadily behind real time, and
   * scheduling on it alone would bank that drift as latency without limit.
   */
  private targetMs(): number {
    const e = this.q[0];
    const due = this.dueMs(e.f.timestamp);
    const paced = this.lastPaintAt === null
      ? due
      : Math.max(due, this.lastPaintAt + (due - this.lastPaintDue) * CATCHUP);
    return Math.min(paced, e.at + this.o.delayMs);
  }

  private schedule(): void {
    if (this.timer !== null) { clearTimeout(this.timer); this.timer = null; }
    if (!this.q.length) return;
    const wait = Math.max(0, this.targetMs() - this.now());
    this.timer = setTimeout(() => { this.timer = null; this.flush(this.now()); }, wait);
  }

  private flush(now: number): void {
    // One frame per slot. A late burst is drained at the capture cadence (a
    // little faster, see CATCHUP) rather than replayed as fast as it arrived:
    // painting the backlog back-to-back is the jump this class exists to remove.
    if (this.q.length && this.targetMs() <= now + 1) {
      const show = this.q.shift()!.f;
      const err = Math.abs(this.now() - this.dueMs(show.timestamp));
      this.counters.jitterMs = this.counters.jitterMs * 0.9 + err * 0.1;
      this.lastPaintDue = this.dueMs(show.timestamp);
      this.lastPaintAt = this.now();
      this.paint(show);
    }
    this.counters.queued = this.q.length;
    this.schedule();
  }

  private paint(frame: VideoFrame): void {
    this.counters.shown++;
    this.o.draw(frame);
  }
}
