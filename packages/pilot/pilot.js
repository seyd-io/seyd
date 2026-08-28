// ── config ─────────────────────────────────────────────────────────────────
const params     = new URLSearchParams(location.search);
const ROBOT_ID   = params.get('robot');
const SIGNAL_URL = params.get('signal');

// QoS: ?qos= wins, then the last choice, then the balanced default.
const QOS_PROFILES = ['latency', 'balanced', 'quality'];
let qosProfile = params.get('qos') || localStorage.getItem('darc.qos') || 'balanced';
if (!QOS_PROFILES.includes(qosProfile)) qosProfile = 'balanced';

// Synthetic loss injection for testing FEC without a cellular link.
// ?loss=0.05 drops 5% of chunks; ?burst=3 drops them in runs of 3 (at 1/3 the
// probability, so mean loss is unchanged and only burstiness varies — that is
// the experiment that says whether real loss is bursty or independent).
const LOSS_RATE  = Math.max(0, Math.min(1, parseFloat(params.get('loss')) || 0));
const LOSS_BURST = Math.max(1, parseInt(params.get('burst'), 10) || 1);

// ── elements ───────────────────────────────────────────────────────────────
const canvas    = document.getElementById('video-canvas');
const statusEl  = document.getElementById('status');
const robotIdEl = document.getElementById('robot-id');
const sensorEl  = document.getElementById('sensor');
const toastEl   = document.getElementById('toast');
const errorEl   = document.getElementById('error');
const errorEgEl = document.getElementById('error-example');
const statsEl   = document.getElementById('stats');
const qosSelect = document.getElementById('qos');
const hintEl    = document.getElementById('hint');

// ── guard: require params ──────────────────────────────────────────────────
if (!ROBOT_ID || !SIGNAL_URL) {
  document.getElementById('container').hidden = true;
  errorEl.hidden = false;
  errorEgEl.textContent =
    `${location.origin}${location.pathname}\n` +
    `  ?robot=mac-robot-01\n` +
    `  &signal=wss://darc-signal-<hash>-ew.a.run.app`;
  throw new Error('Missing required URL parameters.');
}

robotIdEl.textContent = ROBOT_ID;

// ── state ──────────────────────────────────────────────────────────────────
let ws          = null;
let wt          = null;   // WebTransport session
let jsonWriter  = null;   // writable side of the bidi JSON stream
let decoder     = null;
let hasVideo    = false;
let relayMode   = false;  // true when falling back to signal-server relay
let session     = null;   // {candidates, fingerprint, hint} from the last `ready`
let pathLabel   = '—';    // winning candidate label, for the stats panel

const ctx = canvas.getContext('2d');

// ── telemetry ──────────────────────────────────────────────────────────────
// Without these, a degraded picture is indistinguishable between link loss,
// agent-side frame drops, and a decoder that cannot keep up — so nothing about
// video quality can be tuned or even verified. Cumulative counters are the audit
// trail; rates are derived over a sliding window.
const stats = {
  chunksRx: 0, bytesRx: 0, parityRx: 0, chunksDup: 0, chunksBadHeader: 0,
  chunksDropped: 0,           // discarded by the synthetic injector
  chunksMissing: 0,           // never arrived, counted at frame close-out
  framesSeen: 0, framesClean: 0, framesRecovered: 0, framesIncomplete: 0,
  framesTooLate: 0,           // completed after a newer frame already decoded
  keyframesClean: 0, keyframesLost: 0,
  framesDecoded: 0, decodeErrors: 0,
  spread: [],                 // per-frame chunk arrival spread, ms
  degraded: false,            // unrecoverable loss since the last clean keyframe
  agent: null,                // last agent-stats message
};

// Sliding 1s window for rate derivation.
let rateWindow = [];
function noteRate(bytes, parityBytes, frames) {
  rateWindow.push({ t: performance.now(), bytes, parityBytes, frames });
}

function windowRates() {
  const now = performance.now();
  rateWindow = rateWindow.filter(s => now - s.t < 1000);
  if (rateWindow.length < 2) return { kbps: 0, kbpsPayload: 0, fps: 0 };
  const span = (now - rateWindow[0].t) / 1000;
  if (span <= 0) return { kbps: 0, kbpsPayload: 0, fps: 0 };
  let bytes = 0, parity = 0, frames = 0;
  for (const s of rateWindow) { bytes += s.bytes; parity += s.parityBytes; frames += s.frames; }
  return {
    kbps:        Math.round(bytes * 8 / span / 1000),
    kbpsPayload: Math.round((bytes - parity) * 8 / span / 1000),
    fps:         Math.round(frames / span),
  };
}

function percentile(arr, p) {
  if (!arr.length) return 0;
  const s = [...arr].sort((a, b) => a - b);
  return s[Math.min(s.length - 1, Math.floor(s.length * p))];
}

// ── synthetic loss injector ────────────────────────────────────────────────
// Applied at the transport read, before any parsing, so the counters above see
// exactly what real loss looks like. Seeded so runs are comparable.
let lossSeed = 0x2545F491;
function nextRandom() {
  lossSeed ^= lossSeed << 13; lossSeed ^= lossSeed >>> 17; lossSeed ^= lossSeed << 5;
  return ((lossSeed >>> 0) % 1e6) / 1e6;
}
let burstRemaining = 0;
function shouldDrop() {
  if (!LOSS_RATE) return false;
  if (burstRemaining > 0) { burstRemaining--; return true; }
  if (nextRandom() < LOSS_RATE / LOSS_BURST) {
    burstRemaining = LOSS_BURST - 1;
    return true;
  }
  return false;
}

// ── ui helpers ─────────────────────────────────────────────────────────────
function setStatus(text, cls = '') {
  statusEl.textContent = text;
  statusEl.className   = cls;
}

let toastTimer = null;
function showToast(text, durationMs = 2500) {
  toastEl.textContent = text;
  toastEl.classList.add('show');
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => toastEl.classList.remove('show'), durationMs);
}

function showFatalError(heading, body) {
  document.getElementById('container').hidden = true;
  errorEl.querySelector('h2').textContent = heading;
  errorEl.querySelector('p').textContent  = body;
  errorEl.querySelector('code').hidden    = true;
  errorEl.hidden = false;
}

// ── webcodecs video decoder ────────────────────────────────────────────────
function initDecoder() {
  if (decoder) decoder.close();
  hasVideo = false;

  decoder = new VideoDecoder({
    output: (frame) => {
      stats.framesDecoded++;
      if (!hasVideo) {
        hasVideo = true;
        setStatus('Connected', 'connected');
      }
      if (canvas.width !== frame.displayWidth || canvas.height !== frame.displayHeight) {
        canvas.width  = frame.displayWidth;
        canvas.height = frame.displayHeight;
      }
      ctx.drawImage(frame, 0, 0);
      frame.close();
    },
    error: (e) => { stats.decodeErrors++; console.error('VideoDecoder error:', e); },
  });

  decoder.configure({
    codec:                 'avc1.42001f',
    optimizeForLatency:    true,
    // 'no-preference', not 'prefer-hardware'. Chrome treats 'prefer-hardware'
    // as a hard *requirement*: isConfigSupported returns false when no hardware
    // H.264 decoder exists, and configure() then throws OperationError, leaving
    // a black canvas whose only trace is a console error. Measured on this
    // codec string: prefer-hardware → false, no-preference → true, on the same
    // machine. 'no-preference' still gets hardware wherever it exists — the UA
    // prefers it — but degrades to software instead of failing outright. That
    // matters for a public demo, where operators arrive on VMs, remote
    // desktops, and Linux boxes without VA-API.
    hardwareAcceleration:  'no-preference',
  });
}

// Pass a complete reassembled H.264 Annex B access unit to the decoder.
// isKeyframe is provided by the chunk reassembler (from the flags byte).
function handleVideoFrame(data, isKeyframe) {
  if (!decoder || decoder.state !== 'configured') return;
  // 'freeze-until-idr' shows a clean but stale picture rather than a corrupted
  // one; 'continue' keeps decoding through the smear. Either is defensible, so
  // it is a per-profile choice — but a stale image is dangerous too, which is
  // why the degraded border is shown regardless of which is active.
  if (onLossPolicy === 'freeze-until-idr' && stats.degraded && !isKeyframe) return;
  try {
    decoder.decode(new EncodedVideoChunk({
      type:      isKeyframe ? 'key' : 'delta',
      timestamp: performance.now() * 1000,
      data,
    }));
  } catch (e) {
    stats.decodeErrors++;
    console.warn('decode skipped:', e.message);
  }
}

// ── frame reassembly with Reed-Solomon recovery ────────────────────────────
// Wire format and code construction live in fec.js / packages/agent/fec.py.
const CHUNK_SIZE  = DARCFec.MAX_CHUNK_PAYLOAD;
const MAX_REORDER = 4;

const _frames = new Map();   // frameId → assembly state
let _newestId  = null;       // highest frame id seen
let _lastDecodedId = null;   // highest frame id handed to the decoder

// Close-out deadlines, overridden by the agent's qos-applied ack.
let deadlineDelta = 30;
let deadlineKey   = 60;
let onLossPolicy  = 'continue';

function handleVideoChunk(value) {
  stats.chunksRx++;
  stats.bytesRx += value.byteLength;

  const h = DARCFec.parseHeader(value);
  if (!h) { stats.chunksBadHeader++; return; }

  const isParity = h.chunkIdx >= h.n;
  if (isParity) stats.parityRx++;

  // Every frame-id comparison goes through the signed helper. Doing this with
  // raw unsigned arithmetic is what let a single reordered chunk from an older
  // frame delete the frame currently being assembled.
  if (_newestId === null || DARCFec.int16Delta(h.frameId, _newestId) > 0) {
    _newestId = h.frameId;
  }
  if (DARCFec.int16Delta(_newestId, h.frameId) > MAX_REORDER) return;  // too old
  if (_lastDecodedId !== null && DARCFec.int16Delta(h.frameId, _lastDecodedId) <= 0) {
    stats.framesTooLate++;
    return;
  }

  let f = _frames.get(h.frameId);
  if (!f) {
    f = {
      id: h.frameId, n: h.n, k: h.k, lastLen: h.lastLen, isKeyframe: h.isKeyframe,
      data: new Array(h.n).fill(null), parity: new Array(h.k).fill(null),
      dataRx: 0, parityRx: 0,
      firstSeen: performance.now(), lastSeen: performance.now(),
      timer: null, closed: false,
    };
    _frames.set(h.frameId, f);
    stats.framesSeen++;
  }
  if (f.closed) return;
  f.lastSeen = performance.now();

  // The close-out deadline measures *silence*, not elapsed time: it is armed on
  // every chunk, so it fires only once chunks stop arriving. A frame still
  // receiving data has not been lost, however long it is taking.
  //
  // This was previously a total-duration budget armed once at frame creation,
  // which is only survivable when a whole frame lands within one deadline. At
  // 720p a keyframe is ~45-68 KB after parity, so the latency profile's 40 ms
  // demanded ~10 Mbps sustained — trivial on a LAN, impossible on a hotspot.
  // Every keyframe was killed mid-arrival: counted lost, its partial state
  // deleted, its remaining chunks then opening a fresh entry that could never
  // reach n. Delta frames are small enough to have kept completing, so they
  // decoded, hit "a key frame is required after configure()", and the canvas
  // stayed black while loss and decode-error counters climbed.
  //
  // Frames cannot be held indefinitely: finishFrame() closes out anything older
  // as soon as a newer frame decodes, which is the real bound here.
  clearTimeout(f.timer);
  f.timer = setTimeout(() => closeFrame(f, false),
                       f.isKeyframe ? deadlineKey : deadlineDelta);

  // Parity was computed over chunks zero-padded to CHUNK_SIZE, so the short
  // final data chunk must be re-padded before it can take part in recovery.
  const slot = isParity ? h.chunkIdx - h.n : h.chunkIdx;
  const target = isParity ? f.parity : f.data;
  if (slot >= target.length || target[slot]) { stats.chunksDup++; return; }

  let body = h.payload;
  if (body.byteLength < CHUNK_SIZE) {
    const padded = new Uint8Array(CHUNK_SIZE);
    padded.set(body);
    body = padded;
  } else {
    body = body.slice();     // copy: the transport buffer is reused
  }
  target[slot] = body;
  if (isParity) f.parityRx++; else f.dataRx++;

  // Eager recovery: the moment n chunks of any kind are in hand the frame is
  // solvable, so this adds no latency of its own.
  if (f.dataRx === f.n) {
    finishFrame(f, false);
  } else if (f.dataRx + f.parityRx >= f.n && f.k > 0) {
    const recovered = DARCFec.decode(f.data, f.parity);
    if (recovered) {
      f.data = recovered;
      f.dataRx = f.n;
      finishFrame(f, true);
    }
  }
}

function reassemble(f) {
  const total = (f.n - 1) * CHUNK_SIZE + f.lastLen;
  const buf = new Uint8Array(total);
  let off = 0;
  for (let i = 0; i < f.n; i++) {
    const take = (i === f.n - 1) ? f.lastLen : CHUNK_SIZE;
    buf.set(f.data[i].subarray(0, take), off);
    off += take;
  }
  return buf;
}

function finishFrame(f, viaFec) {
  if (f.closed) return;
  f.closed = true;
  clearTimeout(f.timer);
  _frames.delete(f.id);

  if (viaFec) stats.framesRecovered++; else stats.framesClean++;
  if (f.isKeyframe) {
    stats.keyframesClean++;
    stats.degraded = false;   // a clean keyframe resets the reference chain
  }
  stats.spread.push(f.lastSeen - f.firstSeen);
  if (stats.spread.length > 300) stats.spread.shift();

  noteRate(0, 0, 1);
  _lastDecodedId = f.id;
  // Anything still older than this can never be decoded now.
  for (const [id, other] of _frames) {
    if (DARCFec.int16Delta(id, f.id) < 0) closeFrame(other, true);
  }
  handleVideoFrame(reassemble(f), f.isKeyframe);
}

function closeFrame(f, superseded) {
  if (f.closed) return;
  f.closed = true;
  clearTimeout(f.timer);
  _frames.delete(f.id);

  stats.framesIncomplete++;
  stats.chunksMissing += (f.n - f.dataRx);
  if (f.isKeyframe) stats.keyframesLost++;
  // Mark the picture untrustworthy until the next clean keyframe: a lost delta
  // frame corrupts every frame that references it, and the operator must not be
  // shown a smeared image without being told.
  stats.degraded = true;
  if (superseded) return;
}

// ── signal message handler (handshake only in phase 2) ────────────────────
function handleSignalMessage(msg) {
  switch (msg.type) {
    case 'ready':
      // Remembered so a dropped session can be re-established without another
      // signalling round trip.
      session = { candidates: msg.candidates, fingerprint: msg.certFingerprint,
                  hint: msg.p2pHint ?? 'likely' };
      initDecoder();
      connectWebTransport(session.candidates, session.fingerprint, session.hint);
      break;
    case 'cmd-out':
      // Robot→pilot JSON relayed via the signal server (relay mode). Same
      // handler as the bidi stream, so sensor data, acks and telemetry behave
      // identically on both paths.
      if (msg.payload) handleStreamMessage(msg.payload);
      break;
    case 'unreachable':
      showFatalError('Cannot reach robot', msg.reason);
      break;
    case 'peer-disconnected':
      setStatus('Robot disconnected', 'error');
      showToast('Robot disconnected');
      cancelP2PRetry();
      relayMode = false;
      // Cleared before closing so the closure handler treats this as a real
      // disconnect and doesn't try to fall back to relay — the robot is gone.
      session = null;
      if (wt) { wt.close(); wt = null; }
      break;
  }
}

// ── JSON stream handler (sensor data, acks — same msgs as phase 1) ─────────
function handleStreamMessage(msg) {
  switch (msg.type) {
    case 'sensor':
      sensorEl.textContent = msg.data;
      break;
    case 'ack':
      if (msg.cmd === 'snapshot') showToast('Snapshot saved');
      if (msg.cmd === 'ptz-home') {
        showToast(msg.ok ? 'Camera returned home' : 'No home position set');
      }
      break;
    case 'agent-stats':
      // The pilot cannot see a frame whose chunks were all lost, so its own
      // loss estimate is biased low. Pairing chunksRx with the agent's
      // chunks_sent is what makes the number a measurement.
      stats.agent = msg;
      break;
    case 'qos-applied':
      applyQosAck(msg);
      break;
    case 'capabilities':
      applyCapabilities(msg);
      break;
  }
}

function applyQosAck(msg) {
  if (msg.pilot) {
    deadlineDelta = msg.pilot.deadlineDelta ?? deadlineDelta;
    deadlineKey   = msg.pilot.deadlineKey   ?? deadlineKey;
    onLossPolicy  = msg.pilot.onLoss        ?? onLossPolicy;
  }
  activeQos = msg.profile || activeQos;
  qosPublisher = msg.publisher || 'unknown';
  if (qosSelect) qosSelect.value = activeQos;
  showToast(qosPublisher === 'unavailable'
    ? `QoS ${activeQos} (transport only)`
    : `QoS ${activeQos}`);
}

// ── webtransport P2P session ───────────────────────────────────────────────
function hexToBuffer(hex) {
  const bytes = new Uint8Array(hex.length / 2);
  for (let i = 0; i < hex.length; i += 2) bytes[i / 2] = parseInt(hex.slice(i, i + 2), 16);
  return bytes.buffer;
}

// How long P2P is worth waiting for, given what the agent learned about its own
// NAT. A robot behind a symmetric NAT with no router mapping can only ever be
// reached across a shared LAN, so there is no point holding video for ten
// seconds to find that out — but a robot that has a port mapping or a usable
// reflexive address deserves the full window before we give up on it.
function p2pDeadlineMs(hint) {
  switch (hint) {
    case 'none':     return 2_000;
    case 'lan-only': return 4_000;
    default:         return 10_000;   // 'likely'
  }
}

// Race all candidates — first one that completes wt.ready wins.
// Resolves with the winning WebTransport; every other attempt is closed.
//
// This function owns the whole P2P attempt including its deadline. That matters:
// an attempt left running past the deadline can still succeed later, and the
// agent switches its video output to QUIC datagrams the moment a session is
// accepted (agent.py on_wt_connected). If the pilot has already given up and
// moved to relay by then, it has no datagram reader attached and video stops
// dead. So no connection may outlive this call unless it is the returned winner.
//
// Candidates are tried highest-priority first, and split into two waves. Ones
// flagged needsProbe depend on the agent having punched a hole in its NAT, so
// they are held back briefly to let those probes land. Ones that don't — a LAN
// address, an explicitly port-mapped address — are fired immediately, because
// making the common same-network case wait on a hole punch it never needed just
// adds latency to every connect.
function raceWebTransportCandidates(candidates, certFingerprintHex,
                                    { holdMs = 400, timeoutMs = 10_000 } = {}) {
  const opts = {
    serverCertificateHashes: [{
      algorithm: 'sha-256',
      value:     hexToBuffer(certFingerprintHex),
    }],
  };

  const sorted = [...candidates].sort((a, b) => (b.priority ?? 0) - (a.priority ?? 0));

  return new Promise((resolve, reject) => {
    const open    = [];
    const total   = sorted.length;
    let   failed  = 0;
    let   settled = false;
    let   holdTimer = null;

    if (total === 0) { reject(new Error('No valid candidates')); return; }

    const closeAllExcept = (winner) => {
      for (const c of open) {
        if (c !== winner) { try { c.close(); } catch {} }
      }
    };

    const settle = (fn) => {
      settled = true;
      clearTimeout(deadline);
      clearTimeout(holdTimer);
      fn();
    };

    const deadline = setTimeout(() => {
      if (settled) return;
      settle(() => { closeAllExcept(null); reject(new Error('P2P timed out')); });
    }, timeoutMs);

    const noteFailure = (label, err) => {
      console.warn(`Candidate ${label} failed:`, err?.message ?? err);
      if (settled) return;
      if (++failed === total) {
        settle(() => {
          closeAllExcept(null);
          reject(new Error(
            `All ${total} connection candidate(s) failed. ` +
            'The robot may be behind a strict firewall or symmetric NAT.'
          ));
        });
      }
    };

    const launch = (c) => {
      if (settled) return;
      let conn;
      try {
        conn = new WebTransport(c.url, opts);
      } catch (e) {
        noteFailure(c.label, e);
        return;
      }
      // Losing attempts get closed below and are never awaited; swallow their
      // closure rejections so they don't surface as unhandled promise errors.
      conn.closed.catch(() => {});
      open.push(conn);

      conn.ready.then(() => {
        if (settled) { try { conn.close(); } catch {} return; }
        settle(() => {
          console.log(`WebTransport connected via ${c.label} candidate`);
          closeAllExcept(conn);
          resolve({ wt: conn, label: c.label });
        });
      }).catch(e => noteFailure(c.label, e));
    };

    const immediate = sorted.filter(c => !c.needsProbe);
    const delayed   = sorted.filter(c => c.needsProbe);

    immediate.forEach(launch);
    if (delayed.length) holdTimer = setTimeout(() => delayed.forEach(launch), holdMs);
  });
}

async function connectWebTransport(candidates, certFingerprintHex, hint = 'likely') {
  if (!Array.isArray(candidates) || candidates.length === 0) {
    showFatalError('Cannot reach robot', 'No WebTransport candidates received from signal server.');
    return;
  }

  setStatus(`Connecting… (trying ${candidates.length} path${candidates.length > 1 ? 's' : ''})`);

  // Attempt P2P WebTransport. The deadline is enforced inside the race so that
  // no attempt survives it — Chrome's own QUIC timeout can be 30s+, and a
  // straggler that connects after we've moved to relay would silently break
  // video (the agent would switch to datagrams we aren't reading).
  let conn;
  try {
    conn = await raceWebTransportCandidates(candidates, certFingerprintHex,
                                            { timeoutMs: p2pDeadlineMs(hint) });
  } catch (e) {
    // P2P failed (NAT, firewall, timeout) — fall back to signal-server relay,
    // but keep trying for P2P in the background. Relay is a compromise on both
    // latency and cost, so it should never be a one-way door: NAT state,
    // interface, and network all change under a robot in the field.
    console.warn('WebTransport P2P failed, falling back to relay:', e.message);
    relayMode = true;
    setStatus('Waiting for video… (relay)');
    if (ws?.readyState === WebSocket.OPEN) {
      ws.send(JSON.stringify({ type: 'relay-request', robotId: ROBOT_ID }));
    }
    scheduleP2PRetry(candidates, certFingerprintHex, hint);
    return;
  }

  attachSession(conn);
}

// Wire up a freshly established WebTransport session. Split out from the
// connect path because the relay-upgrade retry needs exactly the same wiring.
async function attachSession(conn) {
  cancelP2PRetry();
  const wasRelay = relayMode;
  relayMode = false;
  pathLabel = conn.label;
  wt = conn.wt;

  // Video datagrams: agent → pilot (unreliable, lowest latency)
  (async () => {
    const reader = wt.datagrams.readable.getReader();
    try {
      while (true) {
        const { value, done } = await reader.read();
        if (done) break;
        if (shouldDrop()) { stats.chunksDropped++; continue; }
        handleVideoChunk(value);
      }
    } catch (e) { console.error('datagram reader error:', e); }
  })();

  // Bidirectional JSON stream (pilot creates it, agent uses it for sensor/ack output)
  let stream;
  try {
    stream = await wt.createBidirectionalStream();
  } catch (e) {
    console.error('bidi stream failed:', e);
    return;
  }
  jsonWriter = stream.writable.getWriter();

  // Inbound JSON (sensor data, acks from agent)
  (async () => {
    const reader  = stream.readable.getReader();
    const decoder = new TextDecoder();
    let buf = '';
    try {
      while (true) {
        const { value, done } = await reader.read();
        if (done) break;
        buf += decoder.decode(value, { stream: true });
        const lines = buf.split('\n');
        buf = lines.pop();
        for (const line of lines) {
          if (line.trim()) {
            try { handleStreamMessage(JSON.parse(line)); } catch {}
          }
        }
      }
    } catch {}
  })();

  // Handle WebTransport closure. If signalling is still up the robot is still
  // there and only the P2P path died, so drop to relay rather than leaving the
  // operator with a frozen canvas — then start trying to climb back to P2P.
  const self = conn.wt;
  const onClosed = () => {
    // A superseded session's close fires after a newer one was attached; it
    // must not tear down the connection that replaced it.
    if (wt !== self) return;
    jsonWriter = null;
    wt = null;
    if (ws?.readyState !== WebSocket.OPEN || !session) {
      setStatus('Disconnected', 'error');
      return;
    }
    console.warn('P2P session dropped — falling back to relay');
    relayMode = true;
    initDecoder();                     // resync; the old decoder may have faulted
    setStatus('Reconnecting… (relay)');
    ws.send(JSON.stringify({ type: 'relay-request', robotId: ROBOT_ID }));
    scheduleP2PRetry(session.candidates, session.fingerprint, session.hint);
  };
  wt.closed.then(onClosed).catch(onClosed);

  // Speak first. The agent discovers this stream's id from inbound data, so
  // until we write, nothing it sends on the stream reaches us — an unprompted
  // announcement at connect time is silently discarded. `hello` makes the
  // agent's capabilities reply a response rather than a push, which is the only
  // ordering that is reliable on this path.
  sendCommand({ type: 'hello' });

  if (wasRelay) {
    console.log('upgraded from relay to P2P');
    showToast('Upgraded to direct connection');
  } else {
    setStatus('Waiting for video…');
  }
}

// ── relay → P2P upgrade ────────────────────────────────────────────────────
// While relaying, keep retrying P2P on a slow cadence. The agent only rewires
// its video output when a WebTransport session is actually accepted, so a
// failed retry costs nothing and never disturbs the working relay.
let p2pRetryTimer = null;
const P2P_RETRY_MS = 30_000;

function cancelP2PRetry() {
  clearTimeout(p2pRetryTimer);
  p2pRetryTimer = null;
}

function scheduleP2PRetry(candidates, certFingerprintHex, hint) {
  cancelP2PRetry();
  p2pRetryTimer = setTimeout(async () => {
    if (!relayMode || ws?.readyState !== WebSocket.OPEN) return;

    // Ask the agent to punch a fresh hole first — whatever mapping it opened
    // for the original attempt has long since lapsed.
    ws.send(JSON.stringify({ type: 'probe-request', robotId: ROBOT_ID }));

    let conn;
    try {
      conn = await raceWebTransportCandidates(candidates, certFingerprintHex,
                                              { timeoutMs: p2pDeadlineMs(hint) });
    } catch {
      scheduleP2PRetry(candidates, certFingerprintHex, hint);
      return;
    }
    // Relay may have been torn down while we were connecting.
    if (!relayMode) { try { conn.wt.close(); } catch {} return; }
    attachSession(conn);
  }, P2P_RETRY_MS);
}

// ── commands ───────────────────────────────────────────────────────────────
// Prefer the WebTransport bidi stream; fall back to the signalling socket.
// The fallback is what makes commands work in relay mode at all — there is no
// bidi stream there, and without this PTZ silently does nothing for every
// operator whose robot sits behind carrier NAT. The agent runs both through the
// same handler, so neither path has a private feature set.
async function sendCommand(msg) {
  if (jsonWriter) {
    try {
      await jsonWriter.write(new TextEncoder().encode(JSON.stringify(msg) + '\n'));
    } catch {}
    return;
  }
  if (ws?.readyState === WebSocket.OPEN) {
    ws.send(JSON.stringify({ type: 'cmd', robotId: ROBOT_ID, payload: msg }));
  }
}

function takeSnapshot() {
  if (!canvas.width || !hasVideo) return;
  const ts = Date.now();
  canvas.toBlob((blob) => {
    const url = URL.createObjectURL(blob);
    const a   = document.createElement('a');
    a.href = url; a.download = `darc-snapshot-${ts}.png`; a.click();
    URL.revokeObjectURL(url);
  }, 'image/png');
  sendCommand({ type: 'snapshot', ts });
}

// ── PTZ control ────────────────────────────────────────────────────────────
// Two input surfaces onto one velocity vector. Mouse/touch is a virtual
// joystick over the picture — press where you want to look and the camera moves
// that way, faster the further out you press. Keyboard is the arrow keys, for
// precise nudges the mouse cannot do well.
//
// Velocity, not position: the camera has no idea what is in its frame, so
// "look at this point" is not something either end can compute. Press-and-hold
// with visual feedback is the honest interface for that, and it is what closes
// the loop a prospect is here to feel.
//
// Repeats while held because the agent sends `momentary` commands that expire
// on their own (see camera.py). A dropped stop message therefore costs one
// expiry window instead of leaving the camera panning into its stop.
const PTZ_REPEAT_MS = 200;   // must stay under camera.py's _DURATION_MS (600)
const PTZ_DEADZONE  = 0.12;  // fraction of half-frame ignored around centre
const PTZ_SPEED     = 55;
const PTZ_SPEED_FAST = 100;

let ptzSupported     = false;
let ptzHomeSupported = false;
let ptzTimer   = null;
let ptzSent    = { pan: 0, tilt: 0, zoom: 0 };
let pointerVec = null;       // non-null while a pointer drag owns pan/tilt
let wheelZoom  = 0;
const heldKeys = new Set();

function applyCapabilities(msg) {
  ptzSupported     = !!msg.ptz;
  ptzHomeSupported = !!msg.ptzHome;
  canvas.classList.toggle('ptz', ptzSupported);
  updateHint();
  if (!ptzSupported) stopPtz();
}

// Only advertise controls the robot on the other end actually has. The same
// agent serves a fixed webcam and a PTZ dome, and offering pan/tilt on a webcam
// teaches an operator that the UI lies.
function updateHint() {
  if (!hintEl) return;
  const parts = [];
  if (ptzSupported) {
    parts.push('DRAG or ARROWS — look', 'SHIFT — fast', 'WHEEL or +/− — zoom');
    if (ptzHomeSupported) parts.push('H — home');
  }
  parts.push('SPACE — snapshot', 'S — stats');
  hintEl.textContent = parts.join(' · ');
}

function clampAxis(v) {
  return Math.max(-100, Math.min(100, Math.round(v || 0)));
}

function samePtz(a, b) {
  return a.pan === b.pan && a.tilt === b.tilt && a.zoom === b.zoom;
}

function sendPtz() {
  sendCommand({ type: 'ptz', ...ptzSent });
}

function setPtz(pan, tilt, zoom) {
  const next = { pan: clampAxis(pan), tilt: clampAxis(tilt), zoom: clampAxis(zoom) };
  const changed = !samePtz(next, ptzSent);
  const moving  = next.pan !== 0 || next.tilt !== 0 || next.zoom !== 0;
  ptzSent = next;

  // Stop the repeat before the support check, not after: a robot that reports
  // PTZ and later reports none would otherwise leave this interval running
  // forever, sending commands nothing is listening for.
  if (!moving || !ptzSupported) {
    clearInterval(ptzTimer);
    ptzTimer = null;
  }
  if (!ptzSupported) return;

  if (moving) {
    // Fire on change so a new direction takes effect now rather than at the
    // next tick, then keep renewing the camera's expiry window while held.
    if (changed) sendPtz();
    if (!ptzTimer) ptzTimer = setInterval(sendPtz, PTZ_REPEAT_MS);
    return;
  }

  // One explicit stop. The agent turns this into a `continuous` zero, which
  // halts immediately instead of letting the last momentary window run out.
  if (changed) sendPtz();
}

function keyboardVector() {
  const speed = heldKeys.has('Shift') ? PTZ_SPEED_FAST : PTZ_SPEED;
  let pan = 0, tilt = 0, zoom = 0;
  if (heldKeys.has('ArrowLeft'))  pan  -= speed;
  if (heldKeys.has('ArrowRight')) pan  += speed;
  if (heldKeys.has('ArrowUp'))    tilt += speed;
  if (heldKeys.has('ArrowDown'))  tilt -= speed;
  if (heldKeys.has('Equal'))      zoom += speed;
  if (heldKeys.has('Minus'))      zoom -= speed;
  return { pan, tilt, zoom };
}

// Pointer owns pan/tilt while it is down; zoom always comes from keys or wheel,
// so you can zoom mid-pan without letting go.
function refreshPtz() {
  const keys = keyboardVector();
  const zoom = wheelZoom || keys.zoom;
  if (pointerVec) setPtz(pointerVec.pan, pointerVec.tilt, zoom);
  else            setPtz(keys.pan, keys.tilt, zoom);
}

function stopPtz() {
  heldKeys.clear();
  pointerVec = null;
  wheelZoom  = 0;
  setPtz(0, 0, 0);
}

// Map a pointer position to a velocity, relative to the centre of the picture.
function pointerVector(e) {
  const r = canvas.getBoundingClientRect();
  if (!r.width || !r.height) return { pan: 0, tilt: 0 };
  const nx = ((e.clientX - r.left) / r.width)  * 2 - 1;   // −1 … 1
  const ny = ((e.clientY - r.top)  / r.height) * 2 - 1;
  const mag = Math.hypot(nx, ny);
  if (mag < PTZ_DEADZONE) return { pan: 0, tilt: 0 };
  // Rescale so speed ramps from zero at the deadzone edge rather than jumping
  // straight to 12% — otherwise the control has a step in it right where an
  // operator makes their finest corrections.
  const ramp = Math.min(1, (mag - PTZ_DEADZONE) / (1 - PTZ_DEADZONE)) / mag;
  return {
    pan:  nx * ramp * 100,
    // Screen Y grows downward, the camera's tilt axis grows upward.
    tilt: -ny * ramp * 100,
  };
}

canvas.addEventListener('pointerdown', (e) => {
  if (!ptzSupported) return;
  e.preventDefault();
  // Capture so a drag that leaves the canvas keeps steering and, more
  // importantly, still delivers its pointerup — an uncaptured pointer released
  // outside the element leaves the camera moving.
  try { canvas.setPointerCapture(e.pointerId); } catch {}
  pointerVec = pointerVector(e);
  refreshPtz();
});

canvas.addEventListener('pointermove', (e) => {
  if (!pointerVec) return;
  pointerVec = pointerVector(e);
  refreshPtz();
});

for (const evt of ['pointerup', 'pointercancel', 'lostpointercapture']) {
  canvas.addEventListener(evt, () => {
    if (!pointerVec) return;
    pointerVec = null;
    refreshPtz();
  });
}

let wheelZoomTimer = null;
canvas.addEventListener('wheel', (e) => {
  if (!ptzSupported) return;
  e.preventDefault();
  wheelZoom = e.deltaY < 0 ? PTZ_SPEED : -PTZ_SPEED;
  refreshPtz();
  clearTimeout(wheelZoomTimer);
  // A wheel has no release event, so the zoom is self-cancelling.
  wheelZoomTimer = setTimeout(() => { wheelZoom = 0; refreshPtz(); }, 220);
}, { passive: false });

// Any way of losing the page is a way of losing the stop message. A sleeping
// laptop or a switched tab must not leave a camera panning.
window.addEventListener('blur', stopPtz);
document.addEventListener('visibilitychange', () => { if (document.hidden) stopPtz(); });

// ── QoS selection ──────────────────────────────────────────────────────────
let activeQos    = qosProfile;
let qosPublisher = 'unknown';

function selectQos(name) {
  if (!QOS_PROFILES.includes(name)) return;
  qosProfile = name;
  localStorage.setItem('darc.qos', name);
  // Prefer the WebTransport stream when it exists; otherwise go via signalling,
  // which is the only path that reaches the agent in relay mode.
  if (jsonWriter) {
    sendCommand({ type: 'qos', profile: name });
  } else if (ws?.readyState === WebSocket.OPEN) {
    ws.send(JSON.stringify({ type: 'qos', robotId: ROBOT_ID, profile: name }));
    activeQos = name;
  }
}

if (qosSelect) {
  qosSelect.value = qosProfile;
  qosSelect.addEventListener('change', () => selectQos(qosSelect.value));
}

// ── stats panel ────────────────────────────────────────────────────────────
let statsVisible = localStorage.getItem('darc.stats') === '1';

function colour(value, warn, bad) {
  return value >= bad ? 'bad' : value >= warn ? 'warn' : 'ok';
}

function renderStats() {
  if (statsEl) statsEl.hidden = !statsVisible;
  canvas.classList.toggle('degraded', stats.degraded);
  if (!statsVisible || !statsEl) return;

  const r = windowRates();
  const a = stats.agent;
  const fecPct = r.kbps ? Math.round((r.kbps - r.kbpsPayload) / r.kbps * 100) : 0;

  // Two loss figures, because they answer different questions. `est` is what the
  // pilot can see on its own and is biased low — a frame lost in its entirety
  // leaves no trace. `true` compares against the agent's own send count.
  const seen = stats.chunksMissing + stats.chunksRx;
  const est  = seen ? stats.chunksMissing / seen * 100 : 0;
  const trueLoss = (a && a.chunks_sent)
    ? Math.max(0, (1 - stats.chunksRx / a.chunks_sent) * 100) : null;

  const path = relayMode ? 'relay (via signal)' : `p2p (${pathLabel})`;
  const lossCls = colour(trueLoss ?? est, 1, 3);
  const keyCls  = colour(stats.keyframesLost, 1, 3);

  statsEl.innerHTML =
    `path   ${path}\n` +
    `qos    ${activeQos}${qosPublisher === 'unavailable' ? ' (transport only)' : ''}\n` +
    `video  ${r.kbps} kbps  ${r.fps} fps   fec ${fecPct}%\n` +
    `loss   <span class="${lossCls}">${trueLoss === null ? '—' : trueLoss.toFixed(1) + '% true'}` +
      `  ${est.toFixed(1)}% est</span>   spread p50 ${percentile(stats.spread, 0.5).toFixed(0)}ms` +
      ` p95 ${percentile(stats.spread, 0.95).toFixed(0)}ms\n` +
    `frames ${stats.framesClean} ok  ${stats.framesRecovered} rec  ` +
      `${stats.framesIncomplete} lost  ${stats.framesTooLate} late\n` +
    `key    <span class="${keyCls}">${stats.keyframesClean} ok  ${stats.keyframesLost} lost</span>` +
      `   decodeQ ${decoder ? decoder.decodeQueueSize : 0}  err ${stats.decodeErrors}\n` +
    (a ? `agent  ${a.frames_sent} sent  ${a.frames_dropped_backlog} dropped  ` +
         `${a.frames_skipped_stale} stale  pending ${a.pending_bytes ?? 0}B\n` +
         `link   cwnd ${a.cwnd ?? '—'}  srtt ${a.srtt_ms ?? '—'}ms\n` : '') +
    (LOSS_RATE ? `\nINJECTING ${(LOSS_RATE * 100).toFixed(1)}% LOSS (burst ${LOSS_BURST}) — ` +
                 `${stats.chunksDropped} dropped\n` : '');
}

setInterval(renderStats, 500);

// Report our counters back to the agent so its log is self-contained for
// post-hoc analysis, and so the deferred adaptive-bitrate loop has an input.
setInterval(() => {
  if (!jsonWriter) return;
  const r = windowRates();
  sendCommand({
    type: 'pilot-stats',
    chunksRx: stats.chunksRx, chunksMissing: stats.chunksMissing,
    framesClean: stats.framesClean, framesRecovered: stats.framesRecovered,
    framesIncomplete: stats.framesIncomplete, keyframesLost: stats.keyframesLost,
    kbps: r.kbps, fps: r.fps,
    spreadP95: Math.round(percentile(stats.spread, 0.95)),
    decodeQ: decoder ? decoder.decodeQueueSize : 0,
  });
}, 1000);

// Arrow keys and +/- rather than WASD: 'S' is already the stats toggle, and
// silently stealing it — or moving stats to another key — would be worse than
// using the keys that need no explanation on a camera.
const PTZ_KEYS = new Set(['ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown',
                          'Equal', 'Minus']);

document.addEventListener('keydown', (e) => {
  if (e.repeat) {
    // The OS autorepeat rate is not ours to inherit; PTZ_REPEAT_MS drives the
    // renewals, and held keys are already in the set.
    if (PTZ_KEYS.has(e.code)) e.preventDefault();
    return;
  }
  // Shift is a modifier, so it has to be tracked on its own keydown too —
  // sampling it only when an arrow goes down means pressing Shift mid-gesture
  // does nothing until the operator releases and re-presses the arrow.
  if (e.key === 'Shift') { heldKeys.add('Shift'); refreshPtz(); return; }

  if (PTZ_KEYS.has(e.code)) {
    e.preventDefault();          // arrows would otherwise scroll the page
    if (!ptzSupported) return;
    heldKeys.add(e.code);
    if (e.shiftKey) heldKeys.add('Shift');
    refreshPtz();
    return;
  }
  if (e.code === 'Space') { e.preventDefault(); takeSnapshot(); }
  if (e.code === 'KeyH' && ptzHomeSupported) {
    e.preventDefault();
    sendCommand({ type: 'ptz-home', ts: Date.now() });
  }
  if (e.code === 'KeyS')  {
    statsVisible = !statsVisible;
    localStorage.setItem('darc.stats', statsVisible ? '1' : '0');
    renderStats();
  }
});

document.addEventListener('keyup', (e) => {
  // Shift changes speed rather than direction, so releasing it mid-gesture has
  // to re-evaluate without ending the movement.
  if (e.key === 'Shift') { heldKeys.delete('Shift'); refreshPtz(); return; }
  if (PTZ_KEYS.has(e.code)) {
    heldKeys.delete(e.code);
    refreshPtz();
  }
});

// ── signaling (handshake + presence only) ─────────────────────────────────
function connect() {
  setStatus('Connecting…');
  ws = new WebSocket(SIGNAL_URL);
  ws.binaryType = 'arraybuffer';

  ws.onopen = () => {
    // The profile rides along with connect so the agent has it before the first
    // frame — and so it arrives even when the session ends up on relay, where
    // there is no pilot→robot JSON path.
    ws.send(JSON.stringify({ type: 'connect', robotId: ROBOT_ID, qos: qosProfile }));
    setStatus('Waiting for robot…');
  };

  ws.onmessage = (event) => {
    if (event.data instanceof ArrayBuffer) {
      // Binary video chunk forwarded by the signal server in relay mode.
      if (shouldDrop()) { stats.chunksDropped++; return; }
      handleVideoChunk(new Uint8Array(event.data));
      return;
    }
    handleSignalMessage(JSON.parse(event.data));
  };

  ws.onerror = () => setStatus('Signaling error', 'error');
  ws.onclose = () => {
    if (!wt && !relayMode) setStatus('Signaling closed', 'error');
  };
}

// ── start ──────────────────────────────────────────────────────────────────
connect();
