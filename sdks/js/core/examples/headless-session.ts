// Copyright 2026 Anton Gravestam
// SPDX-License-Identifier: Apache-2.0
// headless-session.ts — a pilot with no UI, on @seyd/core alone.
//
// Connects to a robot, renders the video into a canvas you own, forwards
// sensor messages to your code and sends commands from your controller. This
// is the shape of a customer's own pilot page, and of a supervisor tool that
// watches a fleet without drawing a HUD. Type-checked by the docs build
// (web/docs/scripts/generate.mjs), so it cannot drift from the SDK.
import { SeydSession } from '@seyd/core';
import type { LinkQuality, P2pFailure, PilotStats, SensorEvent } from '@seyd/core';

const canvas = document.querySelector<HTMLCanvasElement>('#video')!;

const session = new SeydSession({
  signalUrl: 'wss://signal.seyd.io/ws',
  canvas,
  // A session token from POST /api/v1/session-tokens, minted by your backend
  // for a signed-in operator. Leave it out only for a robot with a public grant.
  token: undefined,
  clientName: 'my-pilot/1.0',
});

// Sensors arrive as parsed JSON on 'json' channels, raw bytes otherwise.
session.on('sensor', (e: SensorEvent) => {
  console.log(e.channel.name, e.seq, e.data);
});

// The state machine: idle → signaling → waiting-robot → connecting → connected.
session.on('state', ({ state, detail }) => console.log('state', state, detail ?? ''));

// Direct first (ADR 0010). If the race fails and the robot allows it, the
// session is carried by the relay and this event says why — show it.
session.on('relay', ({ failure }: { failure: P2pFailure }) => {
  console.warn('relayed:', failure.reason, failure.natReport);
});
session.on('p2p-failed', (f: P2pFailure) => console.error('no path:', f.reason, f.detail ?? ''));

// Link quality for your own indicator; stats every 500 ms for a HUD.
session.on('link', (q: LinkQuality) => console.log('link', q.state, q.rttMs, q.lossPct));
session.on('stats', (s: PilotStats) => console.log('fps', s.fps, 'g2g p50', s.g2gP50Ms, 'path', s.transport));

// A driver sends commands; an observer's are dropped by the robot. Only a
// channel the robot declared can be written, so check first.
function steer(steering: number, throttle: number): void {
  if (session.hasCommandChannel('drive')) session.send('drive', { steering, throttle });
}

await session.connect('my-robot');
steer(0, 0);

// Tear down: closes the transport and the worker; no event fires afterwards.
window.addEventListener('pagehide', () => session.close());
