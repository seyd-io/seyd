// Copyright 2026 Anton Gravestam
// SPDX-License-Identifier: Apache-2.0
// WebTransport candidate race. Ported from the prototype: highest priority
// first, `needs_probe` candidates held ~400 ms so the agent's NAT probes can
// land, a deadline from the p2p hint, and — load-bearing — every losing or
// late attempt is closed, because the agent switches its video output to a
// session the moment it is accepted.
import { Candidate, FailureReason, P2pHint } from './types.js';

export interface RaceResult { wt: WebTransport; label: string }

export class RaceError extends Error {
  constructor(readonly reason: FailureReason, message: string) { super(message); }
}

export function p2pDeadlineMs(hint: P2pHint | undefined): number {
  switch (hint) {
    case 'none': return 2000;
    case 'lan-only': return 4000;
    default: return 10000;
  }
}

function hexToBuffer(hex: string): ArrayBuffer {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = parseInt(hex.substr(i * 2, 2), 16);
  return out.buffer;
}

export function raceCandidates(
  candidates: Candidate[], fingerprints: string[],
  { holdMs = 400, timeoutMs = 10000 }: { holdMs?: number; timeoutMs?: number } = {},
): Promise<RaceResult> {
  const opts: WebTransportOptions = {
    serverCertificateHashes: fingerprints.map((f) => ({ algorithm: 'sha-256', value: hexToBuffer(f) })),
  };
  const sorted = [...candidates].sort((a, b) => (b.priority ?? 0) - (a.priority ?? 0));

  return new Promise((resolve, reject) => {
    if (sorted.length === 0) { reject(new RaceError('no-candidates', 'No connection candidates')); return; }
    const open: WebTransport[] = [];
    let failed = 0, settled = false;
    let certFailures = 0;
    let holdTimer: ReturnType<typeof setTimeout> | null = null;

    const closeAllExcept = (winner: WebTransport | null) => {
      for (const c of open) if (c !== winner) { try { c.close(); } catch { /* ignore */ } }
    };
    const settle = (fn: () => void) => { settled = true; clearTimeout(deadline); if (holdTimer) clearTimeout(holdTimer); fn(); };
    const deadline = setTimeout(() => {
      if (settled) return;
      settle(() => { closeAllExcept(null); reject(new RaceError('all-candidates-timeout', `No candidate connected within ${timeoutMs} ms`)); });
    }, timeoutMs);

    const noteFailure = (label: string, err: unknown) => {
      const msg = (err as Error)?.message ?? String(err);
      if (/certificate|cert/i.test(msg)) certFailures++;
      if (settled) return;
      if (++failed === sorted.length) {
        settle(() => {
          closeAllExcept(null);
          const reason: FailureReason = certFailures === sorted.length ? 'cert-mismatch' : 'handshake-timeout';
          reject(new RaceError(reason, `All ${sorted.length} candidate(s) failed (last: ${label}: ${msg})`));
        });
      }
    };

    const launch = (c: Candidate) => {
      if (settled) return;
      let conn: WebTransport;
      try { conn = new WebTransport(c.url, opts); } catch (e) { noteFailure(c.label, e); return; }
      conn.closed.catch(() => { /* losers are closed by us; swallow */ });
      open.push(conn);
      conn.ready.then(() => {
        if (settled) { try { conn.close(); } catch { /* ignore */ } return; }
        settle(() => { closeAllExcept(conn); resolve({ wt: conn, label: c.label }); });
      }).catch((e) => noteFailure(c.label, e));
    };

    const immediate = sorted.filter((c) => !c.needs_probe);
    const delayed = sorted.filter((c) => c.needs_probe);
    immediate.forEach(launch);
    if (delayed.length) holdTimer = setTimeout(() => delayed.forEach(launch), holdMs);
  });
}
