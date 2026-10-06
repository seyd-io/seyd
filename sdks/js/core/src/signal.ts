// Copyright 2026 Anton Gravestam
// SPDX-License-Identifier: Apache-2.0
// Pilot-role client for docs/protocol/signal-v2.md.
import { Offer } from './types.js';

export interface SignalEvents {
  open: void;
  'auth-ok': { subject: string };
  offer: Offer;
  'robot-offline': { robot_id: string };
  denied: { reason: string };
  'peer-disconnected': { session_id: string };
  close: { code: number; reason: string };
  error: { message: string };
}

type Handler<T> = (ev: T) => void;

export class SignalClient {
  private ws: WebSocket | null = null;
  private handlers = new Map<string, Set<Handler<unknown>>>();
  private closedByUser = false;
  private backoffMs = 1000;
  subject: string | null = null;

  constructor(readonly url: string, private token?: string) {}

  on<K extends keyof SignalEvents>(type: K, h: Handler<SignalEvents[K]>): () => void {
    let set = this.handlers.get(type);
    if (!set) { set = new Set(); this.handlers.set(type, set); }
    set.add(h as Handler<unknown>);
    return () => set!.delete(h as Handler<unknown>);
  }

  private emit<K extends keyof SignalEvents>(type: K, ev: SignalEvents[K]): void {
    this.handlers.get(type)?.forEach((h) => h(ev));
  }

  connect(): void {
    this.closedByUser = false;
    const ws = new WebSocket(this.url);
    this.ws = ws;
    ws.onopen = () => {
      this.backoffMs = 1000;
      this.send({ type: 'auth', v: 2, role: 'pilot', token: this.token });
      this.emit('open', undefined);
    };
    ws.onmessage = (ev) => {
      if (typeof ev.data !== 'string') return;
      let msg: { type?: string; [k: string]: unknown };
      try { msg = JSON.parse(ev.data); } catch { return; }
      switch (msg.type) {
        case 'auth-ok': this.subject = String(msg.subject ?? ''); this.emit('auth-ok', { subject: this.subject }); break;
        case 'offer': this.emit('offer', msg as unknown as Offer); break;
        case 'robot-offline': this.emit('robot-offline', { robot_id: String(msg.robot_id) }); break;
        case 'denied': this.emit('denied', { reason: String(msg.reason ?? 'denied') }); break;
        case 'peer-disconnected': this.emit('peer-disconnected', { session_id: String(msg.session_id) }); break;
        default: break; // unknown types are ignored
      }
    };
    ws.onerror = () => this.emit('error', { message: 'signaling socket error' });
    ws.onclose = (ev) => {
      if (this.ws !== ws) return;
      this.ws = null;
      this.emit('close', { code: ev.code, reason: ev.reason });
      if (!this.closedByUser) {
        setTimeout(() => { if (!this.closedByUser) this.connect(); }, this.backoffMs);
        this.backoffMs = Math.min(this.backoffMs * 2, 15000);
      }
    };
  }

  get isOpen(): boolean { return !!this.ws && this.ws.readyState === WebSocket.OPEN; }

  send(msg: Record<string, unknown>): boolean {
    if (!this.isOpen) return false;
    this.ws!.send(JSON.stringify(msg));
    return true;
  }

  connectRobot(robotId: string): boolean {
    return this.send({ type: 'connect', robot_id: robotId, client: { kind: 'browser', alpn: 'h3' } });
  }
  retry(sessionId: string): boolean { return this.send({ type: 'retry', session_id: sessionId }); }
  abort(sessionId: string): boolean { return this.send({ type: 'abort', session_id: sessionId }); }
  report(sessionId: string, outcome: 'p2p' | 'relay' | 'failed', extra: Record<string, unknown> = {}): boolean {
    return this.send({ type: 'report', session_id: sessionId, outcome, ...extra });
  }

  close(): void {
    this.closedByUser = true;
    const ws = this.ws;
    this.ws = null;
    if (ws) { try { ws.close(); } catch { /* ignore */ } }
  }
}
