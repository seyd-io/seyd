import '@seyd/web';
import type { SeydSession } from '@seyd/core';
import type { SeydConnectErrorElement, SeydHudElement, SeydVideoElement } from '@seyd/web';
import { PtzController } from './ptz.js';

const params = new URLSearchParams(location.search);
const ROBOT_ID = params.get('robot') || 'seyd-demo';
const SIGNAL_URL = params.get('signal') || (location.hostname === 'localhost' || location.hostname === '127.0.0.1' ? 'ws://localhost:8080/ws' : `${location.protocol === 'https:' ? 'wss' : 'ws'}://${location.host}/ws`);
const QOS_PROFILES = ['latency', 'balanced', 'quality'];
let qos = params.get('qos') || localStorage.getItem('seyd.qos') || 'balanced';
if (!QOS_PROFILES.includes(qos)) qos = 'balanced';

const video = document.getElementById('video') as SeydVideoElement;
const hud = document.getElementById('hud') as SeydHudElement;
const err = document.getElementById('err') as SeydConnectErrorElement;
const qosSelect = document.getElementById('qos') as HTMLSelectElement;
const hintEl = document.getElementById('hint')!;
const sensorEl = document.getElementById('sensor')!;
const roleEl = document.getElementById('role')!;
const lanHintEl = document.getElementById('lan-hint')!;
document.getElementById('robot')!.textContent = ROBOT_ID;
qosSelect.value = qos;

let ptz: PtzController | null = null;

video.addEventListener('seyd-session', (e) => {
  const session = (e as CustomEvent<SeydSession>).detail;
  hud.session = session;
  err.session = session;
  ptz?.dispose();
  ptz = new PtzController(session, video.canvas, video);
  session.on('welcome', ({ role, pathLabel }) => {
    const hasPtz = session.hasCommandChannel('ptz');
    ptz?.setEnabled(hasPtz && role === 'driver');
    roleEl.textContent = role === 'driver' ? 'driver' : 'observer — someone else is driving; controls disabled';
    roleEl.className = `badge ${role}`;
    roleEl.hidden = false;
    // Chrome refuses a public-origin page a direct connection to a private IP
    // (Local Network Access) unless the user allows it; the race then falls
    // through to the hairpin. Only meaningful when a LAN candidate was offered.
    const hadHost = session.candidates.some((c) => c.label === 'host');
    const viaHairpin = pathLabel === 'srflx' || pathLabel === 'portmap';
    lanHintEl.textContent = 'Chrome blocked the direct LAN connection (local network access); if you are on the robot\'s network, allow it in the site permissions for lowest latency.';
    lanHintEl.hidden = !(hadHost && viaHairpin && location.protocol === 'https:');
    hintEl.textContent = (hasPtz ? 'DRAG or ARROWS — look · SHIFT — fast · WHEEL or +/− — zoom · H — home · ' : '') + 'SPACE — snapshot · S — stats';
  });
  session.on('stats', (s) => { const w = window as unknown as { __seydTrace?: string[] }; if (w.__seydTrace && s.trace) w.__seydTrace.push(...s.trace); });
  session.on('sensor', ({ channel, data }) => { sensorEl.textContent = `${channel.name}: ${typeof data === 'string' ? data : JSON.stringify(data)}`; });
  session.on('state', ({ state }) => { if (state !== 'connected') { ptz?.setEnabled(false); roleEl.hidden = true; lanHintEl.hidden = true; } });
});

qosSelect.addEventListener('change', () => {
  qos = qosSelect.value;
  localStorage.setItem('seyd.qos', qos);
  video.session?.setQos(qos);
});

document.addEventListener('keydown', (e) => {
  if (e.code === 'Space' && !e.repeat) { e.preventDefault(); snapshot(); }
});

function snapshot(): void {
  const c = video.canvas;
  if (!c.width || video.session?.state !== 'connected') return;
  // The canvas is rendered by the worker (OffscreenCanvas) — toBlob still works on the placeholder element.
  c.toBlob((blob) => {
    if (!blob) return;
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url; a.download = `seyd-snapshot-${Date.now()}.png`; a.click();
    URL.revokeObjectURL(url);
  }, 'image/png');
}

const loss = parseFloat(params.get('loss') ?? '0') || 0;
if (params.get('paths')) video.setAttribute('paths', params.get('paths')!);
if (params.get('trace')) { video.setAttribute('trace', '1'); (window as unknown as { __seydTrace: string[] }).__seydTrace = []; }
if (loss > 0) { video.setAttribute('loss', String(loss)); video.setAttribute('burst', params.get('burst') ?? '1'); }
video.setAttribute('qos', qos);
video.setAttribute('signal-url', SIGNAL_URL);
video.setAttribute('robot-id', ROBOT_ID);   // last: this attribute set is what starts the session
