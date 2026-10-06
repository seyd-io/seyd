// Copyright 2026 Anton Gravestam
// SPDX-License-Identifier: Apache-2.0
import { PilotStats, SeydSession } from '@seyd/core';

/** <seyd-hud> — the stats overlay. Set `.session`; toggle with `S` or `.toggle()`. */
export class SeydHudElement extends HTMLElement {
  private _session: SeydSession | null = null;
  private pre: HTMLPreElement;
  private _visible = false;
  private last: PilotStats | null = null;
  private unsub: (() => void) | null = null;
  private keyHandler = (e: KeyboardEvent) => { if (e.code === 'KeyS' && !e.metaKey && !e.ctrlKey) this.toggle(); };

  constructor() {
    super();
    const root = this.attachShadow({ mode: 'open' });
    root.innerHTML = `<style>
      :host { display: block; }
      /* Over the picture: scrim tokens, never the theme's surface (docs/design.md). */
      pre {
        margin: 0; padding: 8px 10px; border-radius: var(--seyd-radius, 6px); white-space: pre;
        background: var(--seyd-scrim, rgba(8,14,15,.72)); color: var(--seyd-on-scrim, #e7eeec);
        font: 12px/1.35 var(--seyd-font-mono, "IBM Plex Mono", ui-monospace, Menlo, monospace);
      }
      pre[hidden] { display: none; }
      .ok { color: var(--seyd-accent, #12a37a) } .warn { color: var(--seyd-amber, #d9a441) } .bad { color: var(--seyd-danger, #e0776c) }
    </style><pre hidden></pre>`;
    this.pre = root.querySelector('pre')!;
    try { this._visible = localStorage.getItem('seyd.hud') === '1'; } catch { /* no storage */ }
  }

  connectedCallback(): void { document.addEventListener('keydown', this.keyHandler); this.render(); }
  disconnectedCallback(): void { document.removeEventListener('keydown', this.keyHandler); this.unsub?.(); }

  get session(): SeydSession | null { return this._session; }
  set session(s: SeydSession | null) {
    this.unsub?.();
    this._session = s;
    this.unsub = s ? s.on('stats', (st) => { this.last = st; this.render(); }) : null;
  }

  /** Whether the overlay is shown. Persisted in localStorage by `toggle()`. */
  get visible(): boolean { return this._visible; }

  toggle(): void { this._visible = !this._visible; try { localStorage.setItem('seyd.hud', this._visible ? '1' : '0'); } catch { /* ignore */ } this.render(); }

  private render(): void {
    this.pre.hidden = !this._visible;
    if (!this._visible) return;
    const s = this.last;
    if (!s) { this.pre.textContent = 'waiting for stats…'; return; }
    const col = (v: number, warn: number, bad: number) => (v >= bad ? 'bad' : v >= warn ? 'warn' : 'ok');
    const fecPct = s.kbps ? Math.round(((s.kbps - s.kbpsPayload) / s.kbps) * 100) : 0;
    const loss = s.lossTruePct ?? s.lossEstPct;
    const a = s.agent;
    const g2g = s.g2gP50Ms === null ? '—' : `${s.g2gP50Ms.toFixed(0)}ms p50 ${s.g2gP95Ms!.toFixed(0)}ms p95`;
    // A relayed session is never dressed up as direct: the path line says so,
    // in amber, together with why the direct race failed (ADR 0010).
    const why = this._session?.lastFailure;
    const path = s.transport === 'relay'
      ? `<span class="warn">RELAY via cloud</span>${why ? `  (direct failed: ${why.reason})` : ''}`
      : `p2p (${s.pathLabel ?? '—'})`;
    this.pre.innerHTML =
      `path   ${path}   rtt ${s.rttMs === null ? '—' : s.rttMs.toFixed(1) + 'ms'}\n` +
      `qos    ${s.qos?.profile ?? '—'}${s.qosPublisher === 'unavailable' ? ' (transport only)' : ''}\n` +
      `video  ${s.kbps} kbps  ${s.fps} fps   fec ${fecPct}%   g2g ${g2g}\n` +
      `loss   <span class="${col(loss, 1, 3)}">${s.lossTruePct === null ? '—' : s.lossTruePct.toFixed(1) + '% true'}  ${s.lossEstPct.toFixed(1)}% est</span>` +
      `   spread p50 ${s.spreadP50Ms.toFixed(0)}ms p95 ${s.spreadP95Ms.toFixed(0)}ms\n` +
      `frames ${s.framesClean} ok  ${s.framesRecovered} rec  ${s.framesIncomplete} lost   late parity ${s.chunksLate}   timed out ${s.framesTimedOut ?? 0} (${s.framesTimedOutLate ?? 0} jitter)  deadline ${s.deadlineDeltaMs ?? '-'}ms\n` +
      `key    <span class="${col(s.keyframesLost, 1, 3)}">${s.keyframesClean} ok  ${s.keyframesLost} lost (${s.keyframesTimedOut ?? 0} timed out)</span>   decodeQ ${s.decodeQueue}  err ${s.decodeErrors}  keyreq ${s.keyframesRequested}\n` +
      (a ? `agent  ${a.frames_sent ?? 0} sent  ${a.frames_dropped_backlog ?? 0} dropped  ${a.frames_skipped_stale ?? 0} stale\n` +
           `link   cwnd ${a.cwnd ?? '—'}  rtt ${a.rtt_ms ?? '—'}ms  min ${a.min_rtt_ms ?? '—'}ms  rate ${a.delivery_kbps ?? '—'} kbps\n` +
           (a.abr_bitrate_kbps !== undefined ? `abr    ${a.abr_bitrate_kbps} kbps (ceiling ${a.abr_ceiling_kbps})  fec ${a.abr_fec_delta}/${a.abr_fec_key}  reason ${a.abr_reason}  loss ${a.abr_loss_pct ?? '—'}% (pilot est ${a.abr_loss_pilot_pct ?? '—'}%)\n` : '') : '') +
      (s.injecting ? `\nINJECTING ${(s.injecting.rate * 100).toFixed(1)}% LOSS (burst ${s.injecting.burst}) — ${s.chunksDropped} dropped\n` : '');
  }
}

if (!customElements.get('seyd-hud')) customElements.define('seyd-hud', SeydHudElement);
