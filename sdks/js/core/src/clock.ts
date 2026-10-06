// Copyright 2026 Anton Gravestam
// SPDX-License-Identifier: Apache-2.0
// Pilot↔agent clock offset from ping/pong (docs/protocol/control-stream.md).
export const nowUs = (): number => Math.round(performance.now() * 1000);

export class Clock {
  /** agent_us ≈ pilot_us + offsetUs */
  offsetUs: number | null = null;
  rttUs: number | null = null;
  minRttUs: number | null = null;
  private samples: { offset: number; rtt: number }[] = [];

  onPong(t1: number, t2: number, t3 = nowUs()): void {
    const rtt = t3 - t1;
    if (rtt < 0) return;
    const offset = t2 - (t1 + rtt / 2);
    this.samples.push({ offset, rtt });
    if (this.samples.length > 16) this.samples.shift();
    // Prefer the lowest-RTT samples: they carry the least queueing error.
    const best = [...this.samples].sort((a, b) => a.rtt - b.rtt).slice(0, Math.max(1, this.samples.length >> 1));
    this.offsetUs = best.reduce((s, x) => s + x.offset, 0) / best.length;
    this.rttUs = rtt;
    this.minRttUs = this.minRttUs === null ? rtt : Math.min(this.minRttUs, rtt);
  }

  /**
   * Glass-to-glass estimate for a chunk sent at `sendTs32` (agent µs, low 32
   * bits) observed now on the pilot. null until the offset is known.
   */
  oneWayMs(sendTs32: number, pilotUs = nowUs()): number | null {
    if (this.offsetUs === null) return null;
    const agentNow = pilotUs + this.offsetUs;
    const delta = ((agentNow - sendTs32) % 4294967296 + 4294967296) % 4294967296;
    const signed = delta > 2147483648 ? delta - 4294967296 : delta;
    return signed / 1000;
  }
}
