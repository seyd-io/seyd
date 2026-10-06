// Copyright 2026 Anton Gravestam
// SPDX-License-Identifier: Apache-2.0
// What the engine needs from a transport: datagrams in and out, one control
// byte stream, and a close signal. Two implementations — the WebTransport
// session that is the product, and the cloud relay (ADR 0010) that carries the
// same bytes over a WebSocket when no direct path connected. The engine cannot
// tell them apart by behaviour, only by `kind`, and it shows that kind rather
// than hiding it.
import { TransportKind } from './types.js';

export interface ControlPipe {
  readable: ReadableStream<Uint8Array>;
  writable: WritableStream<Uint8Array>;
}

export interface Transport {
  readonly kind: TransportKind;
  /** Path label reported in stats: the winning candidate's, or 'relay'. */
  readonly label: string;
  readonly datagrams: ReadableStream<Uint8Array>;
  sendDatagram(u8: Uint8Array): void;
  /** The control stream. Opened once; the pilot speaks first on it. */
  openControl(): Promise<ControlPipe>;
  /** Resolves with a reason once the transport is gone, however it went. */
  readonly closed: Promise<string>;
  close(): void;
}

export class WebTransportTransport implements Transport {
  readonly kind = 'p2p' as const;
  readonly datagrams: ReadableStream<Uint8Array>;
  readonly closed: Promise<string>;
  private dgWriter: WritableStreamDefaultWriter<Uint8Array>;

  constructor(private wt: WebTransport, readonly label: string) {
    this.datagrams = wt.datagrams.readable;
    this.dgWriter = wt.datagrams.writable.getWriter();
    this.closed = wt.closed.then(() => 'closed', (e) => String((e as Error)?.message ?? e));
  }
  sendDatagram(u8: Uint8Array): void { this.dgWriter.write(u8).catch(() => { /* closed */ }); }
  async openControl(): Promise<ControlPipe> {
    const s = await this.wt.createBidirectionalStream();
    return { readable: s.readable, writable: s.writable };
  }
  close(): void {
    try { this.dgWriter.releaseLock(); } catch { /* ignore */ }
    try { this.wt.close(); } catch { /* ignore */ }
  }
}

/** First byte of a relay frame; mirrors packages/seyd-transport/src/relay.rs. */
export const RELAY_KIND_DATAGRAM = 1;
export const RELAY_KIND_CONTROL = 2;

/** Outbound bytes a relay socket may hold before commands are dropped rather than queued. */
const RELAY_MAX_BUFFERED = 512 * 1024;

export class RelayError extends Error {
  constructor(readonly reason: 'relay-unavailable', message: string) { super(message); }
}

/**
 * The cloud relay as a Transport. One WebSocket to the signal server's
 * `/relay`; the first message attaches to the session with the token from the
 * offer, and the robot is only dialled by the cloud once we have. Binary
 * frames are `[kind, ...payload]`; nothing else crosses.
 */
export class RelayTransport implements Transport {
  readonly kind = 'relay' as const;
  readonly label = 'relay';
  readonly datagrams: ReadableStream<Uint8Array>;
  readonly closed: Promise<string>;
  private dgCtl!: ReadableStreamDefaultController<Uint8Array>;
  private ctlCtl: ReadableStreamDefaultController<Uint8Array> | null = null;
  private ctlReadable: ReadableStream<Uint8Array>;
  private resolveClosed!: (reason: string) => void;
  private done = false;

  private constructor(private ws: WebSocket) {
    this.datagrams = new ReadableStream<Uint8Array>({ start: (c) => { this.dgCtl = c; } });
    this.ctlReadable = new ReadableStream<Uint8Array>({ start: (c) => { this.ctlCtl = c; } });
    this.closed = new Promise<string>((r) => { this.resolveClosed = r; });
    ws.onmessage = (ev) => this.onMessage(ev);
    ws.onclose = (ev) => this.finish(`relay socket closed (${ev.code}${ev.reason ? ' ' + ev.reason : ''})`);
    ws.onerror = () => this.finish('relay socket error');
  }

  /**
   * Dial and attach. Resolves once the cloud reports the robot attached too,
   * so a `hello` sent right after is delivered; rejects with `RelayError` if
   * the relay refuses, the robot never shows up, or `timeoutMs` passes.
   */
  static connect(url: string, sessionId: string, token: string, { timeoutMs = 15000 }: { timeoutMs?: number } = {}): Promise<RelayTransport> {
    return new Promise((resolve, reject) => {
      let ws: WebSocket;
      try { ws = new WebSocket(url); } catch (e) { reject(new RelayError('relay-unavailable', String((e as Error)?.message ?? e))); return; }
      ws.binaryType = 'arraybuffer';
      let settled = false;
      const fail = (msg: string) => { if (settled) return; settled = true; clearTimeout(timer); try { ws.close(); } catch { /* ignore */ } reject(new RelayError('relay-unavailable', msg)); };
      const timer = setTimeout(() => fail(`relay attach timed out after ${timeoutMs} ms`), timeoutMs);
      ws.onopen = () => ws.send(JSON.stringify({ type: 'relay-attach', session_id: sessionId, token, party: 'pilot' }));
      ws.onerror = () => fail('relay socket error');
      ws.onclose = (ev) => fail(`relay closed before attach (${ev.code}${ev.reason ? ' ' + ev.reason : ''})`);
      ws.onmessage = (ev) => {
        if (typeof ev.data !== 'string') return;
        let msg: { type?: string; reason?: string };
        try { msg = JSON.parse(ev.data); } catch { return; }
        if (msg.type === 'relay-attached') {
          settled = true; clearTimeout(timer);
          resolve(new RelayTransport(ws));
        } else if (msg.type === 'denied' || msg.type === 'relay-closed') {
          fail(`relay ${msg.type}: ${msg.reason ?? 'unknown'}`);
        }
      };
    });
  }

  private onMessage(ev: MessageEvent): void {
    if (typeof ev.data === 'string') {
      let msg: { type?: string; reason?: string };
      try { msg = JSON.parse(ev.data); } catch { return; }
      if (msg.type === 'relay-closed' || msg.type === 'denied') this.finish(`relay ${msg.type}: ${msg.reason ?? 'unknown'}`);
      return;
    }
    const u8 = new Uint8Array(ev.data as ArrayBuffer);
    if (u8.length < 1) return;
    const payload = u8.subarray(1);
    try {
      if (u8[0] === RELAY_KIND_DATAGRAM) this.dgCtl.enqueue(payload);
      else if (u8[0] === RELAY_KIND_CONTROL) this.ctlCtl?.enqueue(payload);
    } catch { /* stream closed */ }
  }

  sendDatagram(u8: Uint8Array): void {
    if (this.ws.readyState !== WebSocket.OPEN || this.ws.bufferedAmount > RELAY_MAX_BUFFERED) return;
    this.ws.send(this.frame(RELAY_KIND_DATAGRAM, u8));
  }

  async openControl(): Promise<ControlPipe> {
    const writable = new WritableStream<Uint8Array>({
      write: (chunk) => { if (this.ws.readyState === WebSocket.OPEN) this.ws.send(this.frame(RELAY_KIND_CONTROL, chunk)); },
    });
    return { readable: this.ctlReadable, writable };
  }

  private frame(kind: number, payload: Uint8Array): Uint8Array {
    const out = new Uint8Array(payload.length + 1);
    out[0] = kind;
    out.set(payload, 1);
    return out;
  }

  private finish(reason: string): void {
    if (this.done) return;
    this.done = true;
    try { this.dgCtl.close(); } catch { /* ignore */ }
    try { this.ctlCtl?.close(); } catch { /* ignore */ }
    this.resolveClosed(reason);
  }

  close(): void {
    try { this.ws.close(1000, 'bye'); } catch { /* ignore */ }
    this.finish('closed');
  }
}
