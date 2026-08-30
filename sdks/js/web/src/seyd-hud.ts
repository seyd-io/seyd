import { PilotStats, SeydSession } from '@seyd/core';

/** <seyd-hud> — the stats overlay. Set `.session`; toggle with `S` or `.toggle()`. */
export class SeydHudElement extends HTMLElement {
  private _session: SeydSession | null = null;
  private pre: HTMLPreElement;
  private visible = false;
  private last: PilotStats | null = null;
  private unsub: (() => void) | null = null;
  private keyHandler = (e: KeyboardEvent) => { if (e.code === 'KeyS' && !e.metaKey && !e.ctrlKey) this.toggle(); };

  constructor() {
    super();
    const root = this.attachShadow({ mode: 'open' });
    root.innerHTML = `<style>
      :host { display: block; }
      pre { margin: 0; padding: 8px 10px; background: rgba(0,0,0,.7); color: #ddd; font: 12px/1.35 ui-monospace, Menlo, monospace; border-radius: 6px; white-space: pre; }
      pre[hidden] { display: none; } .ok { color: #7d7 } .warn { color: #fd6 } .bad { color: #f66 }
    </style><pre hidden></pre>`;
    this.pre = root.querySelector('pre')!;
    try { this.visible = localStorage.getItem('seyd.hud') === '1'; } catch { /* no storage */ }
  }

  connectedCallback(): void { document.addEventListener('keydown', this.keyHandler); this.render(); }
  disconnectedCallback(): void { document.removeEventListener('keydown', this.keyHandler); this.unsub?.(); }

  get session(): SeydSession | null { return this._session; }
  set session(s: SeydSession | null) {
    this.unsub?.();
    this._session = s;
    this.unsub = s ? s.on('stats', (st) => { this.last = st; this.render(); }) : null;
  }

  toggle(): void { this.visible = !this.visible; try { localStorage.setItem('seyd.hud', this.visible ? '1' : '0'); } catch { /* ignore */ } this.render(); }

  private render(): void {
    this.pre.hidden = !this.visible;
    if (!this.visible) return;
    const s = this.last;
    if (!s) { this.pre.textContent = 'waiting for stats…'; return; }
    const col = (v: number, warn: number, bad: number) => (v >= bad ? 'bad' : v >= warn ? 'warn' : 'ok');
    const fecPct = s.kbps ? Math.round(((s.kbps - s.kbpsPayload) / s.kbps) * 100) : 0;
    const loss = s.lossTruePct ?? s.lossEstPct;
    const a = s.agent;
    const g2g = s.g2gP50Ms === null ? '—' : `${s.g2gP50Ms.toFixed(0)}ms p50 ${s.g2gP95Ms!.toFixed(0)}ms p95`;
    this.pre.innerHTML =
      `path   p2p (${s.pathLabel ?? '—'})   rtt ${s.rttMs === null ? '—' : s.rttMs.toFixed(1) + 'ms'}\n` +
      `qos    ${s.qos?.profile ?? '—'}${s.qosPublisher === 'unavailable' ? ' (transport only)' : ''}\n` +
      `video  ${s.kbps} kbps  ${s.fps} fps   fec ${fecPct}%   g2g ${g2g}\n` +
      `loss   <span class="${col(loss, 1, 3)}">${s.lossTruePct === null ? '—' : s.lossTruePct.toFixed(1) + '% true'}  ${s.lossEstPct.toFixed(1)}% est</span>` +
      `   spread p50 ${s.spreadP50Ms.toFixed(0)}ms p95 ${s.spreadP95Ms.toFixed(0)}ms\n` +
      `frames ${s.framesClean} ok  ${s.framesRecovered} rec  ${s.framesIncomplete} lost   late parity ${s.chunksLate}   timed out ${s.framesTimedOut ?? 0}  deadline ${s.deadlineDeltaMs ?? '-'}ms\n` +
      `key    <span class="${col(s.keyframesLost, 1, 3)}">${s.keyframesClean} ok  ${s.keyframesLost} lost</span>   decodeQ ${s.decodeQueue}  err ${s.decodeErrors}  keyreq ${s.keyframesRequested}\n` +
      (a ? `agent  ${a.frames_sent ?? 0} sent  ${a.frames_dropped_backlog ?? 0} dropped  ${a.frames_skipped_stale ?? 0} stale\n` +
           `link   cwnd ${a.cwnd ?? '—'}  rtt ${a.rtt_ms ?? '—'}ms  min ${a.min_rtt_ms ?? '—'}ms  rate ${a.delivery_kbps ?? '—'} kbps\n` +
           (a.abr_bitrate_kbps !== undefined ? `abr    ${a.abr_bitrate_kbps} kbps (ceiling ${a.abr_ceiling_kbps})  fec ${a.abr_fec_delta}/${a.abr_fec_key}  reason ${a.abr_reason}\n` : '') : '') +
      (s.injecting ? `\nINJECTING ${(s.injecting.rate * 100).toFixed(1)}% LOSS (burst ${s.injecting.burst}) — ${s.chunksDropped} dropped\n` : '');
  }
}

if (!customElements.get('seyd-hud')) customElements.define('seyd-hud', SeydHudElement);
