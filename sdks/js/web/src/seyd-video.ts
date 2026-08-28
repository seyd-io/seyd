import { SeydSession, SeydSessionOptions, SessionState } from '@seyd/core';

/**
 * <seyd-video robot-id="seyd-demo" signal-url="wss://signal.seyd.io/ws" channel="main" qos="balanced">
 * Connects P2P on attach, renders the video channel into its canvas, shows a
 * status line, and draws a red border while the picture is degraded.
 * Properties: `.session` (SeydSession), `.canvas`.
 */
export class SeydVideoElement extends HTMLElement {
  static observedAttributes = ['robot-id', 'signal-url', 'qos', 'token', 'loss', 'burst', 'host'];
  session: SeydSession | null = null;
  readonly canvas: HTMLCanvasElement;
  private statusEl: HTMLDivElement;
  private connected = false;

  constructor() {
    super();
    const root = this.attachShadow({ mode: 'open' });
    root.innerHTML = `
      <style>
        :host { display: block; position: relative; background: #000; color: #eee; font: 13px/1.4 system-ui, sans-serif; }
        canvas { display: block; width: 100%; height: 100%; object-fit: contain; touch-action: none; box-sizing: border-box; border: 3px solid transparent; }
        canvas.degraded { border-color: #e33; }
        .status { position: absolute; left: 8px; bottom: 8px; padding: 2px 8px; background: rgba(0,0,0,.6); border-radius: 4px; }
        .status.connected { color: #7d7; } .status.error { color: #f77; }
        ::slotted(*) { position: absolute; }
      </style>
      <canvas></canvas><div class="status">Idle</div><slot></slot>`;
    this.canvas = root.querySelector('canvas')!;
    this.statusEl = root.querySelector('.status')!;
  }

  connectedCallback(): void { this.connected = true; this.start(); }
  disconnectedCallback(): void { this.connected = false; this.stop(); }
  attributeChangedCallback(): void {
    if (!this.connected) return;
    // Attributes commonly arrive after upgrade (frameworks set them one by one);
    // start once both required ones exist, restart if a live session's config changes.
    if (this.session) { this.stop(); this.start(); }
    else if (this.getAttribute('robot-id') && this.getAttribute('signal-url')) this.start();
  }

  private start(): void {
    const robotId = this.getAttribute('robot-id');
    const signalUrl = this.getAttribute('signal-url');
    if (!robotId || !signalUrl) { this.setStatus('Waiting for robot-id and signal-url…', ''); return; }
    const lossRate = parseFloat(this.getAttribute('loss') ?? '0') || 0;
    const opts: SeydSessionOptions = {
      signalUrl, token: this.getAttribute('token') ?? undefined, canvas: this.canvas,
      qos: this.getAttribute('qos'), host: (this.getAttribute('host') as 'auto' | 'worker' | 'inline') ?? 'auto',
      loss: lossRate > 0 ? { rate: Math.min(1, lossRate), burst: Math.max(1, parseInt(this.getAttribute('burst') ?? '1', 10) || 1) } : null,
      clientName: '@seyd/web',
    };
    const s = new SeydSession(opts);
    this.session = s;
    s.on('state', ({ state, detail }) => this.setStatus(this.statusText(state, detail), state === 'connected' ? 'connected' : state === 'p2p-failed' ? 'error' : ''));
    s.on('link', (l) => this.canvas.classList.toggle('degraded', l.degradedPicture));
    s.on('error', (e) => { if (e.fatal) this.setStatus(e.message, 'error'); });
    this.dispatchEvent(new CustomEvent('seyd-session', { detail: s, bubbles: true }));
    void s.connect(robotId);
  }

  private stop(): void { this.session?.close(); this.session = null; }

  private statusText(state: SessionState, detail?: string): string {
    switch (state) {
      case 'signaling': return 'Connecting to signaling…';
      case 'waiting-robot': return detail === 'retrying' ? 'Retrying direct connection…' : 'Waiting for robot…';
      case 'connecting': return `Connecting directly… (${detail ?? ''})`;
      case 'connected': return 'Connected';
      case 'p2p-failed': return 'No direct connection';
      case 'closed': return 'Closed';
      default: return 'Idle';
    }
  }
  private setStatus(text: string, cls: string): void { this.statusEl.textContent = text; this.statusEl.className = `status ${cls}`; }
}

if (!customElements.get('seyd-video')) customElements.define('seyd-video', SeydVideoElement);
