// Copyright 2026 Anton Gravestam
// SPDX-License-Identifier: Apache-2.0
/**
 * The pilot SDK core: `SeydSession` (signalling, the candidate race, the relay as the shown last resort), wire v2 reassembly with Reed-Solomon recovery, WebCodecs decode, presentation pacing and stats. Framework-free; `@seyd/web` builds the custom elements on it.
 *
 * @module @seyd/core
 */
export * from './types.js';
export { SeydSession } from './session.js';
export type { SeydSessionOptions } from './session.js';
export { SignalClient } from './signal.js';
export { Reassembler, MAX_REORDER } from './reassembler.js';
export type { AssembledFrame, LossEvent } from './reassembler.js';
export { Decoder } from './decoder.js';
export { Clock, nowUs } from './clock.js';
export { StatsTracker, percentile } from './stats.js';
export { raceCandidates, p2pDeadlineMs, RaceError } from './race.js';
export { RelayTransport, WebTransportTransport, RelayError } from './transport.js';
export type { Transport } from './transport.js';
export { Engine } from './engine.js';
export type { EngineEvent, EngineOptions } from './engine.js';
export { InlineHost, WorkerHost, workerSupported } from './host.js';
export * as wire from './wire.js';
export * as fec from './fec.js';
export { encodeFrame, BLOCK_DATA_CHUNKS } from './encode.js';
export { parseV1 } from './wire-v1.js';

export { MjpegDecoder } from './mjpeg.js';
