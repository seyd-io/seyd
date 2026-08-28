// SeydSession — the public entry point. Signaling on the main thread, the
// media engine on a worker (or inline). P2P only: a failed race surfaces as a
// `p2p-failed` event with the robot's NatReport, and is retried every 15 s.
import { EngineEvent } from './engine.js';
import { Host, InlineHost, WorkerHost, workerSupported } from './host.js';
import { SignalClient } from './signal.js';
import { ChannelInfo, FailureReason, Offer, P2pFailure, PilotStats, QosInfo, SessionEvents, SessionState } from './types.js';

export interface SeydSessionOptions {
  signalUrl: string;
  token?: string;
  /** Canvas to render the primary video channel into. */
  canvas?: HTMLCanvasElement | null;
  /** Also emit `frame` events (VideoFrame, transferred) — needed if you render yourself. */
  emitFrames?: boolean;
  /** 'worker' (default when supported) or 'inline'. */
  host?: 'auto' | 'worker' | 'inline';
  /** Supply your own Worker (built from `@seyd/core/worker`) if your bundler needs it. */
  worker?: Worker;
  qos?: string | null;
  loss?: { rate: number; burst: number } | null;
  /** Collect a per-frame pipeline trace in `lastStats.trace` (debugging). */
  trace?: boolean;
  /** Debug: only race candidates with these labels (e.g. ['srflx']). */
  paths?: string[] | null;
  clientName?: string;
  retryMs?: number;
  /** Allow more than one live session to the same robot from this document (multi-view). Default: a new session closes the previous one. */
  allowMultiple?: boolean;
}

// One live session per (signal URL, robot) per document unless opted out.
// Two sessions on one page mean two decoders on one canvas and a second
// signaling subject that the cloud gives the observer role — exactly the
// "picture replays / controls dead" failure seen in the field.
const liveSessions = new Map<string, SeydSession>();

type Handler<T> = (ev: T) => void;

export class SeydSession {
  state: SessionState = 'idle';
  channels: ChannelInfo[] = [];
  qos: QosInfo | null = null;
  role: 'driver' | 'observer' | null = null;
  sessionId: string | null = null;
  robotId: string | null = null;
  pathLabel: string | null = null;
  /** Candidates the robot advertised in the last offer (label/url/priority). */
  candidates: Offer['candidates'] = [];
  lastStats: PilotStats | null = null;
  lastFailure: P2pFailure | null = null;

  private signal: SignalClient;
  private host: Host;
  private handlers = new Map<string, Set<Handler<unknown>>>();
  private offer: Offer | null = null;
  private peerGone = false;
  private retryTimer: ReturnType<typeof setTimeout> | null = null;
  private closed = false;
  private decoderText = new TextDecoder();

  constructor(private o: SeydSessionOptions) {
    this.signal = new SignalClient(o.signalUrl, o.token);
    const mode = o.host ?? 'auto';
    const useWorker = mode === 'worker' || (mode === 'auto' && workerSupported());
    let canvas: OffscreenCanvas | HTMLCanvasElement | null = o.canvas ?? null;
    const transfer: Transferable[] = [];
    if (useWorker) {
      // Literal `new Worker(new URL(..., import.meta.url))` so bundlers (Vite,
      // webpack) discover and emit the worker chunk.
      const worker = o.worker ?? new Worker(new URL('./worker.js', import.meta.url), { type: 'module' });
      this.host = new WorkerHost(worker);
      if (o.canvas) { canvas = o.canvas.transferControlToOffscreen(); transfer.push(canvas); }
    } else {
      this.host = new InlineHost();
    }
    this.host.onEvent((ev) => this.onEngine(ev));
    this.host.post({ t: 'init', options: { canvas, emitFrames: !!o.emitFrames, loss: o.loss ?? null, trace: !!o.trace, paths: o.paths ?? null, qosProfile: o.qos ?? null, clientName: o.clientName, token: o.token } }, transfer);

    this.signal.on('auth-ok', () => {
      // A signaling reconnect while the P2P session is racing or live must not
      // ask for a second offer: the QUIC session survives signaling drops.
      if (!this.robotId || this.state === 'connecting' || this.state === 'connected') return;
      this.setState('waiting-robot');
      this.signal.connectRobot(this.robotId);
    });
    this.signal.on('offer', (offer) => this.onOffer(offer));
    this.signal.on('robot-offline', () => this.fail('robot-offline', null));
    this.signal.on('denied', ({ reason }) => { this.emit('error', { message: `signaling denied: ${reason}`, fatal: true }); this.fail('token-rejected', null, reason); });
    // The transport close is what tears the session down; remembering the
    // cloud's notice makes the resulting failure report say 'robot-offline'
    // instead of blaming the re-race ('handshake-timeout').
    this.signal.on('peer-disconnected', () => { this.peerGone = true; });
    this.signal.on('close', () => { if (this.state === 'signaling' || this.state === 'waiting-robot') this.setState('signaling', 'reconnecting'); });
  }

  on<K extends keyof SessionEvents>(type: K, h: Handler<SessionEvents[K]>): () => void {
    let set = this.handlers.get(type);
    if (!set) { set = new Set(); this.handlers.set(type, set); }
    set.add(h as Handler<unknown>);
    return () => set!.delete(h as Handler<unknown>);
  }
  private emit<K extends keyof SessionEvents>(type: K, ev: SessionEvents[K]): void { this.handlers.get(type)?.forEach((h) => h(ev)); }
  private setState(state: SessionState, detail?: string): void { this.state = state; this.emit('state', { state, detail }); }

  async connect(robotId: string): Promise<void> {
    if (this.closed) throw new Error('session is closed');
    this.robotId = robotId;
    const key = `${this.o.signalUrl}|${robotId}`;
    this.liveKey = key;
    if (!this.o.allowMultiple) {
      const prev = liveSessions.get(key);
      if (prev && prev !== this) { console.warn('[seyd] closing previous session to', robotId); prev.close(); }
      liveSessions.set(key, this);
    }
    this.setState('signaling');
    this.signal.connect();
  }
  private liveKey: string | null = null;

  private onOffer(offer: Offer): void {
    this.peerGone = false;
    if (this.closed) return;
    if (this.state === 'connecting' || this.state === 'connected') {
      // Already racing or live: this offer is a duplicate (or a stale retry);
      // taking it would replace sessionId/role with a second session's.
      this.signal.abort(offer.session_id);
      return;
    }
    this.offer = offer;
    this.sessionId = offer.session_id;
    this.role = offer.role;
    this.candidates = offer.candidates ?? [];
    this.channels = offer.channels ?? [];
    this.clearRetry();
    this.host.post({ t: 'connect', offer });
  }

  private fail(reason: FailureReason, failure: P2pFailure | null, detail?: string): void {
    if (this.peerGone && reason !== 'robot-offline') { reason = 'robot-offline'; failure = null; detail = 'robot went offline'; }
    const f: P2pFailure = failure ?? { reason, natReport: this.offer?.nat_report ?? null, candidates: this.offer?.candidates ?? [], detail };
    this.lastFailure = f;
    if (this.sessionId) this.signal.report(this.sessionId, 'failed', { failure_reason: f.reason });
    this.setState('p2p-failed', f.detail ?? f.reason);
    this.emit('p2p-failed', f);
    this.scheduleRetry();
  }

  private scheduleRetry(): void {
    this.clearRetry();
    if (this.closed) return;
    this.retryTimer = setTimeout(() => {
      if (this.closed || !this.robotId) return;
      // Ask the cloud for a fresh offer (and a NAT punch); the offer callback re-races.
      if (this.sessionId) this.signal.retry(this.sessionId);
      this.setState('waiting-robot', 'retrying');
      this.signal.connectRobot(this.robotId);
    }, this.o.retryMs ?? 15000);
  }
  private clearRetry(): void { if (this.retryTimer) clearTimeout(this.retryTimer); this.retryTimer = null; }

  private onEngine(ev: EngineEvent): void {
    switch (ev.t) {
      case 'state':
        if (ev.state === 'connecting') this.setState('connecting', ev.detail);
        else if (ev.state === 'connected') this.setState('connected');
        else if (ev.state === 'closed') { if (this.onClosedAck) { const k = this.onClosedAck; this.onClosedAck = null; k(); } else this.setState('closed'); }
        break;
      case 'welcome':
        this.channels = ev.channels; this.qos = ev.qos; this.role = ev.role; this.pathLabel = ev.pathLabel;
        if (this.sessionId) this.signal.report(this.sessionId, 'p2p', { path_label: ev.pathLabel });
        this.emit('welcome', { sessionId: ev.sessionId, role: ev.role, channels: ev.channels, qos: ev.qos, pathLabel: ev.pathLabel });
        break;
      case 'p2p-failed': this.fail(ev.failure.reason, ev.failure); break;
      case 'sensor': {
        const ch = this.channels.find((c) => c.id === ev.channelId);
        if (!ch) break;
        let data: unknown = ev.raw;
        if (ch.codec === 'json') { try { data = JSON.parse(this.decoderText.decode(ev.raw)); } catch { data = null; } }
        this.emit('sensor', { channel: ch, seq: ev.seq, data, raw: ev.raw, sendTs: ev.sendTs });
        break;
      }
      case 'frame': {
        const ch = this.channels.find((c) => c.id === ev.channelId);
        if (ch) this.emit('frame', { channel: ch, frame: ev.frame }); else ev.frame.close();
        break;
      }
      case 'video-size': this.emit('video-size', { width: ev.width, height: ev.height }); break;
      case 'stats': this.lastStats = ev.stats; this.emit('stats', ev.stats); break;
      case 'link': this.emit('link', ev.link); break;
      case 'error': this.emit('error', { message: ev.message, fatal: ev.fatal }); break;
      case 'control': break;
    }
  }

  /** Send a command on a named command channel. JSON-encodes objects for codec 'json'. */
  send(channel: string, payload: unknown, _opts: { reliable?: boolean } = {}): boolean {
    const ch = this.channels.find((c) => c.name === channel && c.kind === 'command');
    if (!ch || this.state !== 'connected') return false;
    const bytes = payload instanceof Uint8Array ? payload : new TextEncoder().encode(typeof payload === 'string' ? payload : JSON.stringify(payload));
    this.host.post({ t: 'send', channelId: ch.id, payload: bytes }, [bytes.buffer as ArrayBuffer]);
    return true;
  }

  hasCommandChannel(name: string): boolean { return this.channels.some((c) => c.kind === 'command' && c.name === name); }
  setQos(profile: string): void { this.host.post({ t: 'set-qos', profile }); }
  requestKeyframe(): void { this.host.post({ t: 'request-keyframe' }); }

  close(): void {
    if (this.closed) return;
    this.closed = true;
    this.clearRetry();
    if (this.liveKey && liveSessions.get(this.liveKey) === this) liveSessions.delete(this.liveKey);
    if (this.sessionId) this.signal.abort(this.sessionId);
    // Let the engine close the WebTransport before the worker is terminated —
    // terminating first leaves a zombie QUIC session on the robot until idle
    // timeout. The engine answers with state 'closed'; 300 ms is the backstop.
    this.host.post({ t: 'close' });
    const kill = () => { if (this.terminateTimer) clearTimeout(this.terminateTimer); this.terminateTimer = null; this.host.terminate(); };
    this.terminateTimer = setTimeout(kill, 300);
    this.onClosedAck = kill;
    this.signal.close();
    this.setState('closed');
  }
  private terminateTimer: ReturnType<typeof setTimeout> | null = null;
  private onClosedAck: (() => void) | null = null;
}
