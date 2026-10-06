// Copyright 2026 Anton Gravestam
// SPDX-License-Identifier: Apache-2.0
// Worker entry: hosts the Engine. Loaded by WorkerHost via
// `new Worker(new URL('./worker.js', import.meta.url), { type: 'module' })`.
import { Engine, EngineEvent } from './engine.js';
import { HostCommand } from './host.js';

let engine: Engine | null = null;
const sink = (ev: EngineEvent, transfer?: Transferable[]) => (self as unknown as Worker).postMessage(ev, transfer ?? []);

self.onmessage = (e: MessageEvent<HostCommand>) => {
  const cmd = e.data;
  switch (cmd.t) {
    case 'init': engine = new Engine(sink, cmd.options); break;
    case 'connect': void engine?.connect(cmd.offer); break;
    case 'send': engine?.send(cmd.channelId, cmd.payload); break;
    case 'set-qos': engine?.setQos(cmd.profile); break;
    case 'request-keyframe': engine?.requestKeyframe(); break;
    case 'close': engine?.close(); break;
  }
};
