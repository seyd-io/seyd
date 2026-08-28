// The transport-side pipeline: candidate race → datagrams → reassembly → FEC →
// decode → render, plus the control stream. Runs unchanged inside a Web
// Worker (worker.ts) or on the main thread (host.ts InlineHost); the only
// difference is where `sink` delivers events.
import { Clock, nowUs } from './clock.js';
import { ControlMessage, ControlStream } from './control.js';
import { Decoder } from './decoder.js';
import { AssembledFrame, Reassembler } from './reassembler.js';
import { RaceError, p2pDeadlineMs, raceCandidates } from './race.js';
import { StatsTracker } from './stats.js';
import { ChannelInfo, LinkQuality, Offer, P2pFailure, PilotStats, QosInfo } from './types.js';
import { encodeMessage, parseChunk } from './wire.js';

export type EngineEvent =
  | { t: 'state'; state: 'connecting' | 'connected' | 'p2p-failed' | 'closed'; detail?: string }
  | { t: 'welcome'; sessionId: string; role: 'driver' | 'observer'; channels: ChannelInfo[]; qos: QosInfo; pathLabel: string }
  | { t: 'sensor'; channelId: number; seq: number; raw: Uint8Array; sendTs: number }
  | { t: 'frame'; channelId: number; frame: VideoFrame }
  | { t: 'video-size'; width: number; height: number }
  | { t: 'stats'; stats: PilotStats }
  | { t: 'link'; link: LinkQuality }
  | { t: 'p2p-failed'; failure: P2pFailure }
  | { t: 'error'; message: string; fatal: boolean }
  | { t: 'control'; msg: ControlMessage };

export interface EngineOptions {
  canvas?: OffscreenCanvas | HTMLCanvasElement | null;
  emitFrames?: boolean;
  loss?: { rate: number; burst: number } | null;
  qosProfile?: string | null;
  clientName?: string;
  token?: string;
}

const MAX_TRANSFER_HISTORY = 0;

export class Engine {
  private wt: WebTransport | null = null;
  private dgWriter: WritableStreamDefaultWriter<Uint8Array> | null = null;
  private control: ControlStream | null = null;
  private reassemblers = new Map<number, Reassembler>();
  private decoder: Decoder | null = null;
  private videoChannel: ChannelInfo | null = null;
  private channels: ChannelInfo[] = [];
  private qos: QosInfo | null = null;
  private qosPublisher: string | null = null;
  private clock = new Clock();
  private stats = new StatsTracker();
  private degraded = false;
  private pathLabel: string | null = null;
  private sessionId: string | null = null;
  private ctx: OffscreenCanvasRenderingContext2D | CanvasRenderingContext2D | null = null;
  private timers: ReturnType<typeof setInterval>[] = [];
  private seqs = new Map<number, number>();
  private closed = false;
  private offer: Offer | null = null;
  // Synthetic loss injection: xorshift so runs are reproducible.
  private rngState = 0x9e3779b9;
  private burstLeft = 0;

  constructor(private sink: (ev: EngineEvent, transfer?: Transferable[]) => void, private o: EngineOptions) {
    if (o.canvas) this.ctx = (o.canvas as OffscreenCanvas).getContext('2d') as OffscreenCanvasRenderingContext2D;
  }

  // ── connection ──────────────────────────────────────────────────────────

  async connect(offer: Offer): Promise<void> {
    this.offer = offer;
    this.sessionId = offer.session_id;
    this.channels = offer.channels ?? [];
    this.sink({ t: 'state', state: 'connecting', detail: `trying ${offer.candidates?.length ?? 0} path(s)` });
    let res;
    try {
      res = await raceCandidates(offer.candidates ?? [], offer.cert_fingerprints ?? [], { timeoutMs: p2pDeadlineMs(offer.p2p_hint) });
    } catch (e) {
      const err = e instanceof RaceError ? e : new RaceError('handshake-timeout', String((e as Error)?.message ?? e));
      this.sink({ t: 'p2p-failed', failure: { reason: err.reason, natReport: offer.nat_report ?? null, candidates: offer.candidates ?? [], detail: err.message } });
      this.sink({ t: 'state', state: 'p2p-failed', detail: err.message });
      return;
    }
    if (this.closed) { try { res.wt.close(); } catch { /* ignore */ } return; }
    await this.attach(res.wt, res.label);
  }

  private async attach(wt: WebTransport, label: string): Promise<void> {
    this.wt = wt;
    this.pathLabel = label;
    this.dgWriter = wt.datagrams.writable.getWriter();
    void this.datagramLoop(wt);

    this.control = new ControlStream((m) => this.onControl(m), () => this.onTransportClosed('control stream closed'));
    try { await this.control.open(wt); } catch (e) { this.sink({ t: 'error', message: `control stream: ${(e as Error).message}`, fatal: true }); return; }
    wt.closed.then(() => this.onTransportClosed('closed')).catch((e) => this.onTransportClosed(String(e?.message ?? e)));

    // Pilot speaks first — the agent learns the stream id from this line.
    this.control.send({ type: 'hello', proto: 2, session_id: this.sessionId, client: { kind: 'browser', name: this.o.clientName ?? '@seyd/core', version: '0.1.0' }, token: this.o.token });
    this.timers.push(setInterval(() => this.control?.send({ type: 'ping', t1: nowUs() }), 1000));
    this.timers.push(setInterval(() => this.emitStats(), 500));
    this.timers.push(setInterval(() => this.sendPilotStats(), 1000));
  }

  private onTransportClosed(detail: string): void {
    if (this.closed || !this.wt) return;
    this.teardownTransport();
    this.sink({ t: 'state', state: 'p2p-failed', detail });
    this.sink({ t: 'p2p-failed', failure: { reason: 'handshake-timeout', natReport: this.offer?.nat_report ?? null, candidates: this.offer?.candidates ?? [], detail: `session dropped: ${detail}` } });
  }

  private teardownTransport(): void {
    for (const t of this.timers) clearInterval(t);
    this.timers = [];
    void this.control?.close();
    this.control = null;
    try { this.dgWriter?.releaseLock(); } catch { /* ignore */ }
    this.dgWriter = null;
    const wt = this.wt;
    this.wt = null;
    if (wt) { try { wt.close(); } catch { /* ignore */ } }
    for (const r of this.reassemblers.values()) r.reset();
    this.decoder?.close();
    this.decoder = null;
    this.videoChannel = null;
  }

  close(): void {
    this.closed = true;
    this.control?.send({ type: 'bye' });
    this.teardownTransport();
    this.sink({ t: 'state', state: 'closed' });
  }

  // ── control stream ──────────────────────────────────────────────────────

  private onControl(m: ControlMessage): void {
    switch (m.type) {
      case 'welcome': {
        this.channels = (m.channels as ChannelInfo[]) ?? this.channels;
        this.qos = (m.qos as QosInfo) ?? null;
        this.setupChannels();
        this.sink({ t: 'welcome', sessionId: String(m.session_id), role: (m.role as 'driver' | 'observer') ?? 'observer', channels: this.channels, qos: this.qos!, pathLabel: this.pathLabel ?? '' });
        this.sink({ t: 'state', state: 'connected' });
        if (this.o.qosProfile && this.qos && this.o.qosProfile !== this.qos.profile) this.setQos(this.o.qosProfile);
        if (this.videoChannel) this.control?.send({ type: 'request-keyframe', ch: this.videoChannel.id });
        break;
      }
      case 'denied':
        this.sink({ t: 'error', message: `agent denied session: ${m.reason}`, fatal: true });
        break;
      case 'pong':
        this.clock.onPong(Number(m.t1), Number(m.t2));
        break;
      case 'qos-ack':
        this.qos = (m.qos as QosInfo) ?? this.qos;
        this.qosPublisher = (m.publisher as string) ?? null;
        this.applyQos();
        break;
      case 'agent-stats':
        this.stats.noteAgentStats(m as never);
        break;
      default:
        this.sink({ t: 'control', msg: m });
    }
  }

  private setupChannels(): void {
    const video = this.channels.find((c) => c.kind === 'video') ?? null;
    if (video && (!this.videoChannel || this.videoChannel.id !== video.id || this.videoChannel.codec !== video.codec)) {
      this.videoChannel = video;
      this.decoder?.close();
      this.decoder = new Decoder({
        codec: video.codec || 'avc1.42001f',
        onFrame: (f) => this.onDecoded(f),
        onError: (e) => this.sink({ t: 'error', message: `decoder: ${e.message}`, fatal: false }),
        onNeedKeyframe: () => this.control?.send({ type: 'request-keyframe', ch: video.id }),
      });
      try { this.decoder.configure(); } catch (e) { this.sink({ t: 'error', message: `decoder configure: ${(e as Error).message}`, fatal: true }); }
    }
    this.applyQos();
  }

  private applyQos(): void {
    if (!this.qos) return;
    for (const r of this.reassemblers.values()) { r.deadlineDeltaMs = this.qos.deadline_delta_ms; r.deadlineKeyMs = this.qos.deadline_key_ms; }
  }

  private reassemblerFor(channelId: number): Reassembler {
    let r = this.reassemblers.get(channelId);
    if (!r) {
      r = new Reassembler({
        deadlineDeltaMs: this.qos?.deadline_delta_ms ?? 30,
        deadlineKeyMs: this.qos?.deadline_key_ms ?? 60,
        onFrame: (f) => this.onFrame(channelId, f),
        onLoss: (l) => {
          this.degraded = true;
          this.control?.send({ type: 'loss', ch: channelId, frame_id: l.frameId, key: l.keyframe });
        },
      });
      this.reassemblers.set(channelId, r);
    }
    return r;
  }

  setQos(profile: string): void {
    this.control?.send({ type: 'set-qos', profile });
  }

  requestKeyframe(): void {
    if (this.videoChannel) this.control?.send({ type: 'request-keyframe', ch: this.videoChannel.id });
  }

  // ── datagrams ───────────────────────────────────────────────────────────

  private shouldDrop(): boolean {
    const l = this.o.loss;
    if (!l || l.rate <= 0) return false;
    if (this.burstLeft > 0) { this.burstLeft--; return true; }
    let x = this.rngState;
    x ^= x << 13; x ^= x >>> 17; x ^= x << 5;
    this.rngState = x >>> 0;
    if (this.rngState / 4294967296 < l.rate / l.burst) { this.burstLeft = l.burst - 1; return true; }
    return false;
  }

  private async datagramLoop(wt: WebTransport): Promise<void> {
    const reader = wt.datagrams.readable.getReader();
    try {
      for (;;) {
        const { value, done } = await reader.read();
        if (done || this.wt !== wt) break;
        if (this.shouldDrop()) { this.stats.chunksDropped++; continue; }
        this.onDatagram(value);
      }
    } catch { /* transport closed; wt.closed handles it */ }
  }

  private onDatagram(u8: Uint8Array): void {
    this.stats.chunksRx++;
    this.stats.bytesRx += u8.byteLength;
    const p = parseChunk(u8);
    if (!p) { this.stats.chunksBadHeader++; return; }
    const h = p.header;
    const ch = this.channels.find((c) => c.id === h.channelId);
    if (h.chunkIdx >= h.n) this.stats.parityRx++;
    if (ch?.kind === 'sensor') {
      this.sink({ t: 'sensor', channelId: h.channelId, seq: h.frameId, raw: p.payload.slice(), sendTs: h.sendTs });
      return;
    }
    if (!ch || ch.kind !== 'video') return;
    this.stats.note(u8.byteLength, h.chunkIdx >= h.n ? u8.byteLength : 0, 0);
    this.reassemblerFor(h.channelId).push(h, p.payload);
  }

  private onFrame(channelId: number, f: AssembledFrame): void {
    this.stats.note(0, 0, 1);
    this.stats.noteSpread(f.lastSeenMs - f.firstSeenMs);
    const g2g = this.clock.oneWayMs(f.sendTsFirst);
    if (g2g !== null) this.stats.noteG2g(g2g);
    if (f.keyframe) this.degraded = false;
    if (!this.decoder) return;
    if (this.qos?.on_loss === 'freeze-until-idr' && this.degraded && !f.keyframe) return;
    this.decoder.decode(f.data, f.keyframe, nowUs());
  }

  private onDecoded(frame: VideoFrame): void {
    this.stats.framesDecoded++;
    const canvas = this.o.canvas;
    if (canvas && this.ctx) {
      if (canvas.width !== frame.displayWidth || canvas.height !== frame.displayHeight) {
        canvas.width = frame.displayWidth;
        canvas.height = frame.displayHeight;
        this.sink({ t: 'video-size', width: canvas.width, height: canvas.height });
      }
      this.ctx.drawImage(frame, 0, 0);
    }
    if (this.o.emitFrames && this.videoChannel) {
      const clone = canvas ? frame.clone() : frame;
      this.sink({ t: 'frame', channelId: this.videoChannel.id, frame: clone }, [clone as unknown as Transferable]);
      if (canvas) frame.close();
    } else {
      frame.close();
    }
  }

  send(channelId: number, payload: Uint8Array): boolean {
    if (!this.dgWriter) return false;
    const seq = (this.seqs.get(channelId) ?? 0) + 1;
    this.seqs.set(channelId, seq);
    this.dgWriter.write(encodeMessage(channelId, seq, payload, nowUs())).catch(() => { /* closed */ });
    return true;
  }

  // ── stats ───────────────────────────────────────────────────────────────

  private snapshot(): PilotStats {
    const r = this.videoChannel ? this.reassemblerFor(this.videoChannel.id).counters : new Reassembler({ deadlineDeltaMs: 0, deadlineKeyMs: 0, onFrame: () => {}, onLoss: () => {} }).counters;
    return this.stats.snapshot({
      reassembler: r,
      decodeErrors: this.decoder?.errors ?? 0, keyframesRequested: this.decoder?.keyframesRequested ?? 0,
      decodeQueue: this.decoder?.queueSize ?? 0, degraded: this.degraded,
      rttMs: this.clock.rttUs === null ? null : this.clock.rttUs / 1000, offsetUs: this.clock.offsetUs,
      pathLabel: this.pathLabel, qos: this.qos, qosPublisher: this.qosPublisher, injecting: this.o.loss && this.o.loss.rate > 0 ? this.o.loss : null,
    });
  }

  private emitStats(): void {
    const s = this.snapshot();
    this.sink({ t: 'stats', stats: s });
    const loss = s.lossTruePct ?? s.lossEstPct;
    const state: LinkQuality['state'] = !this.wt ? 'lost' : loss >= 5 || s.keyframesLost > 0 && this.degraded ? 'poor' : loss >= 1 || this.degraded ? 'degraded' : 'good';
    this.sink({ t: 'link', link: { state, rttMs: s.rttMs, offsetUs: s.offsetUs, lossPct: loss, degradedPicture: this.degraded } });
  }

  private sendPilotStats(): void {
    const s = this.snapshot();
    this.control?.send({
      type: 'pilot-stats', chunks_rx: s.chunksRx, chunks_missing: s.chunksMissing, frames_clean: s.framesClean,
      frames_recovered: s.framesRecovered, frames_incomplete: s.framesIncomplete, keyframes_lost: s.keyframesLost,
      kbps: s.kbps, fps: s.fps, g2g_ms: s.g2gP50Ms, decode_q: s.decodeQueue,
    });
  }
}
void MAX_TRANSFER_HISTORY;
