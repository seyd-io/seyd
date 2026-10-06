// Copyright 2026 Anton Gravestam
// SPDX-License-Identifier: Apache-2.0
// Where the Engine runs. WorkerHost puts transport+FEC+decode+render on a
// Web Worker with an OffscreenCanvas so host-page jank cannot delay video;
// InlineHost runs the same Engine on the main thread where OffscreenCanvas or
// module workers are unavailable.
import { Engine, EngineEvent, EngineOptions } from './engine.js';
import { Offer } from './types.js';

export type HostCommand =
  | { t: 'init'; options: EngineOptions }
  | { t: 'connect'; offer: Offer }
  | { t: 'send'; channelId: number; payload: Uint8Array }
  | { t: 'set-qos'; profile: string }
  | { t: 'request-keyframe' }
  | { t: 'close' };

export interface Host {
  post(cmd: HostCommand, transfer?: Transferable[]): void;
  onEvent(cb: (ev: EngineEvent) => void): void;
  terminate(): void;
}

export class InlineHost implements Host {
  private engine: Engine | null = null;
  private cb: ((ev: EngineEvent) => void) | null = null;
  post(cmd: HostCommand): void {
    switch (cmd.t) {
      case 'init': this.engine = new Engine((ev) => this.cb?.(ev), cmd.options); break;
      case 'connect': void this.engine?.connect(cmd.offer); break;
      case 'send': this.engine?.send(cmd.channelId, cmd.payload); break;
      case 'set-qos': this.engine?.setQos(cmd.profile); break;
      case 'request-keyframe': this.engine?.requestKeyframe(); break;
      case 'close': this.engine?.close(); break;
    }
  }
  onEvent(cb: (ev: EngineEvent) => void): void { this.cb = cb; }
  terminate(): void { this.engine?.close(); this.engine = null; }
}

export class WorkerHost implements Host {
  constructor(private worker: Worker) {}
  post(cmd: HostCommand, transfer: Transferable[] = []): void { this.worker.postMessage(cmd, transfer); }
  onEvent(cb: (ev: EngineEvent) => void): void { this.worker.onmessage = (e: MessageEvent<EngineEvent>) => cb(e.data); }
  terminate(): void { this.worker.terminate(); }
}

export function workerSupported(): boolean {
  return typeof Worker !== 'undefined' && typeof OffscreenCanvas !== 'undefined' &&
    typeof (HTMLCanvasElement.prototype as { transferControlToOffscreen?: unknown }).transferControlToOffscreen === 'function';
}
