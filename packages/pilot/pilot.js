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
      initDecoder();
      connectWebTransport(msg.candidates, msg.certFingerprint);
      break;
    case 'unreachable':
      showFatalError('Cannot reach robot', msg.reason);
      break;
    case 'peer-disconnected':
      setStatus('Robot disconnected', 'error');
      showToast('Robot disconnected');
      relayMode = false;
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

// Race all candidates in parallel — first one that completes wt.ready wins.
// Returns the winning WebTransport object; closes all others.
// Waits holdMs before attempting, giving the agent time to send hole-punch probes.
async function raceWebTransportCandidates(candidates, certFingerprintHex, holdMs = 400) {
  const opts = {
    serverCertificateHashes: [{
      algorithm: 'sha-256',
      value:     hexToBuffer(certFingerprintHex),
    }],
  };

  // Give the agent's NAT hole-punch probes time to open the agent's NAT before
  // we fire QUIC Initial packets. 400ms is generous for a round-trip through
  // the signal server plus probe transmission.
  await new Promise(r => setTimeout(r, holdMs));

  const connections = candidates.map(c => {
    try { return { wt: new WebTransport(c.url, opts), label: c.label }; }
    catch { return null; }
  }).filter(Boolean);

  if (connections.length === 0) throw new Error('No valid candidates');

  return new Promise((resolve, reject) => {
    let pending  = connections.length;
    let resolved = false;

    connections.forEach(({ wt: conn, label }, i) => {
      conn.ready.then(() => {
        if (!resolved) {
          resolved = true;
          console.log(`WebTransport connected via ${label} candidate`);
          // Close all other in-flight connections
          connections.forEach(({ wt: c }, j) => { if (j !== i) c.close(); });
          resolve({ wt: conn, label });
        } else {
          conn.close();
        }
      }).catch(err => {
        console.warn(`Candidate ${label} failed:`, err.message);
        pending--;
        if (pending === 0 && !resolved) {
          reject(new Error(
            `All ${connections.length} connection candidate(s) failed. ` +
            'The robot may be behind a strict firewall or symmetric NAT.'
          ));
        }
      });
    });
  });
}

async function connectWebTransport(candidates, certFingerprintHex) {
  if (!Array.isArray(candidates) || candidates.length === 0) {
    showFatalError('Cannot reach robot', 'No WebTransport candidates received from signal server.');
    return;
  }

  setStatus(`Connecting… (trying ${candidates.length} path${candidates.length > 1 ? 's' : ''})`);

  // Attempt P2P WebTransport. Cap the wait — Chrome's QUIC timeout can be 30s+,
  // which is too long before falling back to relay.
  const P2P_TIMEOUT_MS = 10_000;
  let conn;
  try {
    conn = await Promise.race([
      raceWebTransportCandidates(candidates, certFingerprintHex),
      new Promise((_, reject) =>
        setTimeout(() => reject(new Error('P2P timed out')), P2P_TIMEOUT_MS)
      ),
    ]);
  } catch (e) {
    // P2P failed (NAT, firewall, timeout) — fall back to signal-server relay.
    console.warn('WebTransport P2P failed, falling back to relay:', e.message);
    relayMode = true;
    setStatus('Waiting for video… (relay)');
    if (ws?.readyState === WebSocket.OPEN) {
      ws.send(JSON.stringify({ type: 'relay-request', robotId: ROBOT_ID }));
    }
    return;
  }

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

  // Handle WebTransport closure
  wt.closed.then(() => {
    setStatus('Disconnected', 'error');
    jsonWriter = null;
    wt = null;
  }).catch(() => {});

  setStatus('Waiting for video…');
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
