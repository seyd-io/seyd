import { SeydSession, SeydSessionOptions, SessionState } from '@seyd/core';

/**
 * <seyd-video robot-id="seyd-demo" signal-url="wss://signal.seyd.io/ws" channel="main" qos="balanced">
 * Connects P2P on attach, renders the video channel into its canvas, shows a
 * status line, and draws a red border while the picture is degraded. When no
 * direct path connects and the robot allows it, the session is carried by the
 * cloud relay (ADR 0010): the status line says so and an amber RELAY badge
 * stays up for as long as it lasts. `relay="0"` disables the fallback.
 * Properties: `.session` (SeydSession), `.canvas`.
 */
export class SeydVideoElement extends HTMLElement {
  static observedAttributes = ['robot-id', 'signal-url', 'qos', 'token', 'loss', 'burst', 'host', 'trace', 'paths', 'presentation-delay', 'relay'];
  session: SeydSession | null = null;
  readonly canvas: HTMLCanvasElement;
  private statusEl: HTMLDivElement;
  private relayBadge: HTMLDivElement;
  private connected = false;
  private restartQueued = false;
  private readonly onPageHide = () => this.stop();
  private readonly onPrerenderingChange = () => this.start();

  constructor() {
    super();
    const root = this.attachShadow({ mode: 'open' });
    root.innerHTML = `
      <style>
        /* Overlays sit on the picture, so they use the scrim tokens, which do not flip
           with the theme (docs/design.md). Fallbacks equal the tokens' values. */
        :host {
          display: block; position: relative; background: #000; color: var(--seyd-on-scrim, #e7eeec);
          font: 12.5px/1.4 var(--seyd-font-body, "IBM Plex Sans", system-ui, sans-serif);
        }
        canvas { display: block; width: 100%; height: 100%; object-fit: contain; touch-action: none; box-sizing: border-box; border: 3px solid transparent; }
        canvas.degraded { border-color: var(--seyd-danger, #e0776c); }
        .status {
          position: absolute; left: 8px; bottom: 8px; padding: 3px 9px; border-radius: var(--seyd-radius-sm, 3px);
          background: var(--seyd-scrim, rgba(8,14,15,.72)); color: var(--seyd-on-scrim-2, #b4c2bf);
          font-family: var(--seyd-font-mono, "IBM Plex Mono", ui-monospace, monospace); font-size: 11.5px;
        }
        .status.connected { color: var(--seyd-accent, #12a37a); }
        .status.error { color: var(--seyd-danger, #e0776c); }
        .status.relay { color: var(--seyd-amber, #d9a441); }
        .relay-badge {
          position: absolute; right: 8px; top: 8px; padding: 3px 9px; border-radius: 999px;
          background: var(--seyd-scrim, rgba(8,14,15,.72)); color: var(--seyd-amber, #d9a441); border: 1px solid var(--seyd-amber, #d9a441);
          font-family: var(--seyd-font-mono, "IBM Plex Mono", ui-monospace, monospace); font-size: 10.5px; letter-spacing: .06em; text-transform: uppercase;
        }
        .relay-badge[hidden] { display: none; }
        ::slotted(*) { position: absolute; }
      </style>
      <canvas></canvas><div class="status">Idle</div><div class="relay-badge" hidden title="No direct connection; video is carried through the Seyd cloud relay (higher latency)">RELAY via cloud</div><slot></slot>`;
    this.canvas = root.querySelector('canvas')!;
    this.statusEl = root.querySelector('.status')!;
    this.relayBadge = root.querySelector('.relay-badge')!;
  }

  private startScheduled = false;
  private liveKey: string | null = null;

  connectedCallback(): void { this.connected = true; this.scheduleStart(); }
  disconnectedCallback(): void { this.connected = false; this.stop(); }
  attributeChangedCallback(name: string): void {
    if (!this.connected) return;
    // `qos` changes apply live; anything else is coalesced into one (re)start.
    if (name === 'qos' && this.session && this.getAttribute('qos')) { this.session.setQos(this.getAttribute('qos')!); return; }
    this.scheduleStart();
  }

  // Attributes commonly arrive one by one after upgrade (frameworks, or a
  // script setting robot-id, signal-url, qos in sequence). Every setAttribute
  // used to spawn a fresh session and leave the previous one running — two
  // decoders on one canvas and a zombie driver session on the robot. Coalesce
  // to a single start per task, and restart only when the identity changes.
  private scheduleStart(): void {
    if (this.startScheduled) return;
    this.startScheduled = true;
    queueMicrotask(() => {
      this.startScheduled = false;
      if (!this.connected) return;
      const key = ['signal-url', 'robot-id', 'token', 'loss', 'burst', 'host', 'trace', 'paths', 'presentation-delay', 'relay'].map((a) => this.getAttribute(a)).join('|');
      if (this.session && key === this.liveKey) return;
      this.stop();
      this.liveKey = key;
      this.start();
    });
  }

  private start(): void {
    const robotId = this.getAttribute('robot-id');
    const signalUrl = this.getAttribute('signal-url');
    if (!robotId || !signalUrl) { this.setStatus('Waiting for robot-id and signal-url…', ''); return; }
    // Chrome may prerender this page from an omnibox prediction; a prerendered
    // document must not take a driver slot the user never asked for.
    if ((document as Document & { prerendering?: boolean }).prerendering) {
      document.addEventListener('prerenderingchange', this.onPrerenderingChange, { once: true });
      this.setStatus('Waiting for page to be shown…', '');
      return;
    }
    if (this.session) return;
    const lossRate = parseFloat(this.getAttribute('loss') ?? '0') || 0;
    const opts: SeydSessionOptions = {
      signalUrl, token: this.getAttribute('token') ?? undefined, canvas: this.canvas,
      qos: this.getAttribute('qos'), host: (this.getAttribute('host') as 'auto' | 'worker' | 'inline') ?? 'auto',
      loss: lossRate > 0 ? { rate: Math.min(1, lossRate), burst: Math.max(1, parseInt(this.getAttribute('burst') ?? '1', 10) || 1) } : null,
      clientName: '@seyd/web', trace: this.hasAttribute('trace'), paths: this.getAttribute('paths')?.split(',').filter(Boolean) ?? null,
      relay: !['0', 'false', 'off'].includes((this.getAttribute('relay') ?? '').toLowerCase()),
    };
    const pd = this.getAttribute('presentation-delay');
    if (pd !== null && pd !== '' && Number.isFinite(Number(pd))) opts.presentationDelayMs = Math.max(0, Number(pd));
    const s = new SeydSession(opts);
    this.session = s;
    s.on('state', ({ state, detail }) => {
      const relayed = s.transport === 'relay';
      this.setStatus(this.statusText(state, detail, relayed), state === 'connected' ? (relayed ? 'relay' : 'connected') : state === 'p2p-failed' ? 'error' : '');
      this.relayBadge.hidden = !(state === 'connected' && relayed);
    });
    s.on('link', (l) => this.canvas.classList.toggle('degraded', l.degradedPicture));
    s.on('error', (e) => { if (e.fatal) this.setStatus(e.message, 'error'); });
    this.dispatchEvent(new CustomEvent('seyd-session', { detail: s, bubbles: true }));
    void s.connect(robotId);
  }

  private stop(): void { this.session?.close(); this.session = null; this.liveKey = null; this.relayBadge.hidden = true; }

  private statusText(state: SessionState, detail?: string, relayed = false): string {
    switch (state) {
      case 'signaling': return 'Connecting to signaling…';
      case 'waiting-robot': return detail === 'retrying' ? 'Retrying direct connection…' : 'Waiting for robot…';
      case 'connecting': return detail === 'relay' ? 'No direct connection — relaying via Seyd cloud…' : `Connecting directly… (${detail ?? ''})`;
      case 'connected': return relayed ? 'Connected via cloud relay (no direct path)' : 'Connected';
      case 'p2p-failed': return detail?.includes('relay') ? 'No direct connection, and the relay failed' : 'No direct connection';
      case 'closed': return 'Closed';
      default: return 'Idle';
    }
  }
  private setStatus(text: string, cls: string): void { this.statusEl.textContent = text; this.statusEl.className = `status ${cls}`; }
}

if (!customElements.get('seyd-video')) customElements.define('seyd-video', SeydVideoElement);
