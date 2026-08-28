# @seyd/core

The Seyd pilot SDK core: signaling, WebTransport candidate race, wire v2
reassembly with per-block Reed-Solomon recovery, WebCodecs decode, the control
stream (clock sync, loss reports, QoS) and stats. Runs the media pipeline on a
Web Worker with an OffscreenCanvas where supported.

```ts
import { SeydSession } from '@seyd/core';
const s = new SeydSession({ signalUrl: 'wss://signal.seyd.io/ws', canvas });
s.on('sensor', ({ channel, data }) => console.log(channel.name, data));
s.on('p2p-failed', (f) => console.warn(f.reason, f.natReport));
s.send('ptz', { pan: 20, tilt: 0, zoom: 0 });
await s.connect('seyd-demo');
```

Contracts: `docs/protocol/`. Conformance: `pnpm test` replays the Python FEC
vectors (`tools/fec-vectors.py`) through the TypeScript coder.
