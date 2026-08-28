import { AgentStats, PilotStats, QosInfo } from './types.js';

export function percentile(arr: number[], p: number): number {
  if (!arr.length) return 0;
  const s = [...arr].sort((a, b) => a - b);
  return s[Math.min(s.length - 1, Math.floor(p * (s.length - 1)))];
}

/** Sliding-window rates plus cumulative counters, mirroring the prototype HUD. */
export class StatsTracker {
  chunksRx = 0; bytesRx = 0; parityRx = 0; chunksBadHeader = 0; chunksDropped = 0;
  framesDecoded = 0;
  spread: number[] = [];
  g2g: number[] = [];
  agent: AgentStats | null = null;
  // (t, agent chunks_sent, pilot chunksRx) at each agent-stats arrival; both
  // are lifetime counters and the agent's may predate this session, so true
  // loss is computed from deltas over a short window, never from totals.
  private lossSamples: { t: number; sent: number; rx: number }[] = [];
  private window: { t: number; bytes: number; parity: number; frames: number }[] = [];

  note(bytes: number, parity: number, frames: number, t = performance.now()): void {
    this.window.push({ t, bytes, parity, frames });
    const cutoff = t - 1000;
    while (this.window.length && this.window[0].t < cutoff) this.window.shift();
  }

  noteAgentStats(a: AgentStats, t = performance.now()): void {
    this.agent = a;
    if (typeof a.chunks_sent === 'number') {
      this.lossSamples.push({ t, sent: a.chunks_sent, rx: this.chunksRx });
      while (this.lossSamples.length > 1 && this.lossSamples[0].t < t - 5000) this.lossSamples.shift();
    }
  }

  /** Windowed true loss %, or null until two agent-stats samples exist. */
  lossTruePct(): number | null {
    if (this.lossSamples.length < 2) return null;
    const a = this.lossSamples[0], b = this.lossSamples[this.lossSamples.length - 1];
    const sent = b.sent - a.sent, rx = b.rx - a.rx;
    if (sent <= 0) return null;
    return Math.max(0, Math.min(100, (1 - rx / sent) * 100));
  }

  noteSpread(ms: number): void { this.spread.push(ms); if (this.spread.length > 300) this.spread.shift(); }
  noteG2g(ms: number): void { this.g2g.push(ms); if (this.g2g.length > 300) this.g2g.shift(); }

  rates(t = performance.now()): { kbps: number; kbpsPayload: number; fps: number } {
    const cutoff = t - 1000;
    let bytes = 0, parity = 0, frames = 0;
    for (const w of this.window) if (w.t >= cutoff) { bytes += w.bytes; parity += w.parity; frames += w.frames; }
    return { kbps: Math.round(bytes * 8 / 1000), kbpsPayload: Math.round((bytes - parity) * 8 / 1000), fps: frames };
  }

  snapshot(extra: {
    reassembler: { chunksDup: number; chunksTooOld: number; chunksTooLate: number; chunksMissing: number; framesSeen: number; framesClean: number; framesRecovered: number; framesIncomplete: number; keyframesClean: number; keyframesLost: number };
    decodeErrors: number; keyframesRequested: number; decodeQueue: number; degraded: boolean;
    rttMs: number | null; offsetUs: number | null; pathLabel: string | null; qos: QosInfo | null; qosPublisher: string | null;
    injecting: { rate: number; burst: number } | null;
  }): PilotStats {
    const r = this.rates();
    const seen = extra.reassembler.chunksMissing + this.chunksRx;
    const lossEst = seen ? extra.reassembler.chunksMissing / seen * 100 : 0;
    const a = this.agent;
    const lossTrue = this.lossTruePct();
    const g2gP50 = this.g2g.length ? percentile(this.g2g, 0.5) : null;
    const g2gP95 = this.g2g.length ? percentile(this.g2g, 0.95) : null;
    return {
      chunksRx: this.chunksRx, bytesRx: this.bytesRx, parityRx: this.parityRx, chunksDup: extra.reassembler.chunksDup,
      chunksBadHeader: this.chunksBadHeader, chunksDropped: this.chunksDropped, chunksMissing: extra.reassembler.chunksMissing,
      chunksTooOld: extra.reassembler.chunksTooOld,
      framesSeen: extra.reassembler.framesSeen, framesClean: extra.reassembler.framesClean,
      framesRecovered: extra.reassembler.framesRecovered, framesIncomplete: extra.reassembler.framesIncomplete,
      chunksLate: extra.reassembler.chunksTooLate,
      keyframesClean: extra.reassembler.keyframesClean, keyframesLost: extra.reassembler.keyframesLost,
      framesDecoded: this.framesDecoded, decodeErrors: extra.decodeErrors, keyframesRequested: extra.keyframesRequested,
      degraded: extra.degraded,
      kbps: r.kbps, kbpsPayload: r.kbpsPayload, fps: r.fps,
      spreadP50Ms: percentile(this.spread, 0.5), spreadP95Ms: percentile(this.spread, 0.95),
      g2gP50Ms: g2gP50, g2gP95Ms: g2gP95,
      rttMs: extra.rttMs, offsetUs: extra.offsetUs, decodeQueue: extra.decodeQueue,
      lossEstPct: lossEst, lossTruePct: lossTrue, pathLabel: extra.pathLabel, qos: extra.qos, qosPublisher: extra.qosPublisher,
      agent: a, injecting: extra.injecting,
      path: extra.pathLabel, lossTrue, g2gP50, g2gP95, rtt: extra.rttMs,
    };
  }
}
