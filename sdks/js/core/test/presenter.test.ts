import { beforeEach, describe, expect, it, vi } from 'vitest';
import { Presenter } from '../src/presenter.js';

// A stand-in for VideoFrame: the presenter only reads `timestamp` and closes.
class FakeFrame {
  closed = false;
  constructor(public timestamp: number) {}
  close(): void { this.closed = true; }
}
const frame = (us: number) => new FakeFrame(us) as unknown as VideoFrame;

function make(delayMs: number) {
  const shown: number[] = [];
  const at: number[] = [];
  const p = new Presenter({
    delayMs,
    now: () => performance.now(),
    draw: (f) => { shown.push(f.timestamp); at.push(performance.now()); (f as unknown as FakeFrame).close(); },
  });
  return { p, shown, at };
}

describe('Presenter', () => {
  beforeEach(() => { vi.useFakeTimers(); vi.setSystemTime(0); });

  it('paints immediately when the delay is zero', () => {
    const { p, shown } = make(0);
    p.push(frame(1000));
    p.push(frame(41000));
    expect(shown).toEqual([1000, 41000]);
  });

  it('paints on the capture timeline, one delay behind', () => {
    const { p, shown, at } = make(50);
    const t0 = performance.now();
    p.push(frame(0));
    expect(shown).toEqual([]);          // held, not painted on arrival
    vi.advanceTimersByTime(50);
    expect(shown).toEqual([0]);
    expect(at[0] - t0).toBeCloseTo(50, 0);

    // Next frame captured 40 ms later, but delivered in a burst right now.
    p.push(frame(40000));
    expect(shown).toEqual([0]);
    vi.advanceTimersByTime(40);
    expect(shown).toEqual([0, 40000]);
  });

  it('absorbs a late keyframe without a hitch on the glass', () => {
    const { p, shown, at } = make(50);
    // Frames captured every 40 ms; the third arrives 35 ms late (the keyframe
    // burst), and the fourth catches up right behind it — the measured shape.
    p.push(frame(0));       vi.advanceTimersByTime(40);
    p.push(frame(40000));   vi.advanceTimersByTime(40);
    vi.advanceTimersByTime(35);
    p.push(frame(80000));
    p.push(frame(120000));
    vi.advanceTimersByTime(200);
    expect(shown).toEqual([0, 40000, 80000, 120000]);   // nothing dropped
    // Paint intervals stay near the capture cadence despite the arrival burst.
    const gaps = at.slice(1).map((t, i) => t - at[i]);
    for (const g of gaps) { expect(g).toBeGreaterThan(30); expect(g).toBeLessThan(50); }
  });

  it('rebuilds the mapping when the agent clock restarts at zero', () => {
    const { p, shown } = make(50);
    p.push(frame(5_000_000));
    vi.advanceTimersByTime(50);
    expect(shown).toEqual([5_000_000]);
    p.push(frame(0));               // seydd restarted: capture clock back to ~0
    vi.advanceTimersByTime(50);
    expect(shown).toEqual([5_000_000, 0]);
    expect(p.counters.reanchors).toBe(1);
  });

  it('pays out a stalled burst in order and within the delay budget', () => {
    // A backlog deeper than the budget cannot be both spaced at the capture
    // cadence and shown on time — 5 frames at 40 ms needs 200 ms of buffer, and
    // only 50 ms was bought. The deadline wins: nothing is dropped or reordered,
    // and no frame is held longer than the delay past its own arrival.
    const { p, shown, at } = make(50);
    p.push(frame(0));
    vi.advanceTimersByTime(50);
    vi.advanceTimersByTime(200);                       // the stall
    const burstAt = performance.now();
    for (let i = 1; i <= 5; i++) p.push(frame(i * 40000));
    vi.advanceTimersByTime(400);
    expect(shown).toEqual([0, 40000, 80000, 120000, 160000, 200000]);
    expect(p.counters.skippedLate).toBe(0);
    // 50 ms budget plus the flush tolerance and timer granularity.
    for (const t of at.slice(1)) expect(t - burstAt).toBeLessThanOrEqual(55);
  });

  it('never costs more than the configured delay, even on a slow source clock', () => {
    // The camera stamps 40 ms per frame but really delivers one every 44 ms,
    // so its timeline walks steadily behind ours (the demo camera is ~1.6 %
    // slow). Pacing on capture alone would bank that drift as latency.
    const { p, shown, at } = make(50);
    const arrivedAt: number[] = [];
    for (let i = 0; i < 200; i++) {
      arrivedAt.push(performance.now());
      p.push(frame(i * 40000));
      vi.advanceTimersByTime(44);
    }
    vi.advanceTimersByTime(500);
    expect(shown.length).toBeGreaterThan(190);
    const latency = at.map((t, i) => t - arrivedAt[i]);
    expect(Math.max(...latency.slice(20))).toBeLessThanOrEqual(51);
  });

  it('closes queued frames on reset rather than leaking them', () => {
    const { p } = make(50);
    const held = new FakeFrame(0);
    p.push(held as unknown as VideoFrame);
    expect(held.closed).toBe(false);
    p.reset();
    expect(held.closed).toBe(true);
    expect(p.counters.queued).toBe(0);
  });
});
