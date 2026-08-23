// ── config ─────────────────────────────────────────────────────────────────
const params     = new URLSearchParams(location.search);
const ROBOT_ID   = params.get('robot');
const SIGNAL_URL = params.get('signal');

// ── elements ───────────────────────────────────────────────────────────────
const canvas    = document.getElementById('video-canvas');
const statusEl  = document.getElementById('status');
const robotIdEl = document.getElementById('robot-id');
const sensorEl  = document.getElementById('sensor');
const toastEl   = document.getElementById('toast');
const errorEl   = document.getElementById('error');
const errorEgEl = document.getElementById('error-example');

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

const ctx = canvas.getContext('2d');

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
    error: (e) => console.error('VideoDecoder error:', e),
  });

  decoder.configure({
    codec:                 'avc1.42001f',
    optimizeForLatency:    true,
    hardwareAcceleration:  'prefer-hardware',
  });
}

// Pass a complete reassembled H.264 Annex B access unit to the decoder.
// isKeyframe is provided by the chunk reassembler (from the flags byte).
function handleVideoFrame(data, isKeyframe) {
  if (!decoder || decoder.state !== 'configured') return;
  try {
    decoder.decode(new EncodedVideoChunk({
      type:      isKeyframe ? 'key' : 'delta',
      timestamp: performance.now() * 1000,
      data,
    }));
  } catch (e) {
    console.warn('decode skipped:', e.message);
  }
}

// Chunk wire format (matches peer.py _blocking_video_relay):
//   byte 0:     flags — bit 7 = keyframe
//   bytes 1-2:  frame_id  (uint16 BE, rolls at 65535)
//   bytes 3-4:  chunk_idx (uint16 BE, 0-based)
//   bytes 5-6:  total_chunks (uint16 BE)
//   bytes 7+:   H.264 Annex B payload slice
const _frameChunks = new Map(); // frame_id → {chunks, received, total, isKeyframe}

function handleVideoChunk(value) {
  // value is a Uint8Array from wt.datagrams.readable
  if (value.byteLength < 7) return;
  const dv          = new DataView(value.buffer, value.byteOffset, value.byteLength);
  const flags       = dv.getUint8(0);
  const frameId     = dv.getUint16(1);
  const chunkIdx    = dv.getUint16(3);
  const totalChunks = dv.getUint16(5);
  const isKeyframe  = (flags & 0x80) !== 0;
  const chunkData   = value.subarray(7); // zero-copy view of this chunk's payload

  if (!_frameChunks.has(frameId)) {
    _frameChunks.set(frameId, { chunks: new Array(totalChunks), received: 0, total: totalChunks, isKeyframe });
    // Evict frames whose IDs are more than 30 behind the current one (stale/lost).
    for (const [id] of _frameChunks) {
      if (((frameId - id) & 0xFFFF) > 30) _frameChunks.delete(id);
    }
  }

  const frame = _frameChunks.get(frameId);
  if (!frame || frame.chunks[chunkIdx]) return; // duplicate chunk
  frame.chunks[chunkIdx] = chunkData.slice(); // copy before buffer is reused
  frame.received++;

  if (frame.received === frame.total) {
    _frameChunks.delete(frameId);
    // Reassemble all chunks into one contiguous Uint8Array
    const totalLen = frame.chunks.reduce((n, c) => n + c.byteLength, 0);
    const buf = new Uint8Array(totalLen);
    let off = 0;
    for (const c of frame.chunks) { buf.set(c, off); off += c.byteLength; }
    handleVideoFrame(buf, frame.isKeyframe);
  }
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
      break;
  }
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
  wt = conn.wt;

  // Video datagrams: agent → pilot (unreliable, lowest latency)
  (async () => {
    const reader = wt.datagrams.readable.getReader();
    try {
      while (true) {
        const { value, done } = await reader.read();
        if (done) break;
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
async function sendCommand(msg) {
  if (!jsonWriter) return;
  try {
    await jsonWriter.write(new TextEncoder().encode(JSON.stringify(msg) + '\n'));
  } catch {}
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

document.addEventListener('keydown', (e) => {
  if (e.code === 'Space') { e.preventDefault(); takeSnapshot(); }
});

// ── signaling (handshake + presence only) ─────────────────────────────────
function connect() {
  setStatus('Connecting…');
  ws = new WebSocket(SIGNAL_URL);
  ws.binaryType = 'arraybuffer';

  ws.onopen = () => {
    ws.send(JSON.stringify({ type: 'connect', robotId: ROBOT_ID }));
    setStatus('Waiting for robot…');
  };

  ws.onmessage = (event) => {
    if (event.data instanceof ArrayBuffer) {
      // Binary video chunk forwarded by the signal server in relay mode.
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
