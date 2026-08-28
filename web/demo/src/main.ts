import '@seyd/web';
import type { SeydSession } from '@seyd/core';
import type { SeydConnectErrorElement, SeydHudElement, SeydVideoElement } from '@seyd/web';
import { PtzController } from './ptz.js';

const params = new URLSearchParams(location.search);
const ROBOT_ID = params.get('robot') || 'seyd-demo';
const SIGNAL_URL = params.get('signal') || (location.hostname === 'localhost' || location.hostname === '127.0.0.1' ? 'ws://localhost:8080/ws' : 'wss://signal.seyd.io/ws');
const QOS_PROFILES = ['latency', 'balanced', 'quality'];
let qos = params.get('qos') || localStorage.getItem('seyd.qos') || 'balanced';
if (!QOS_PROFILES.includes(qos)) qos = 'balanced';

const video = document.getElementById('video') as SeydVideoElement;
const hud = document.getElementById('hud') as SeydHudElement;
const err = document.getElementById('err') as SeydConnectErrorElement;
const qosSelect = document.getElementById('qos') as HTMLSelectElement;
const hintEl = document.getElementById('hint')!;
const sensorEl = document.getElementById('sensor')!;
document.getElementById('robot')!.textContent = ROBOT_ID;
qosSelect.value = qos;

let ptz: PtzController | null = null;

video.addEventListener('seyd-session', (e) => {
  const session = (e as CustomEvent<SeydSession>).detail;
  hud.session = session;
  err.session = session;
  ptz?.dispose();
  ptz = new PtzController(session, video.canvas, video);
  session.on('welcome', () => {
    const hasPtz = session.hasCommandChannel('ptz');
    ptz?.setEnabled(hasPtz);
    hintEl.textContent = (hasPtz ? 'DRAG or ARROWS — look · SHIFT — fast · WHEEL or +/− — zoom · H — home · ' : '') + 'SPACE — snapshot · S — stats';
  });
  session.on('sensor', ({ channel, data }) => { sensorEl.textContent = `${channel.name}: ${typeof data === 'string' ? data : JSON.stringify(data)}`; });
  session.on('state', ({ state }) => { if (state !== 'connected') ptz?.setEnabled(false); });
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
if (loss > 0) { video.setAttribute('loss', String(loss)); video.setAttribute('burst', params.get('burst') ?? '1'); }
video.setAttribute('qos', qos);
video.setAttribute('signal-url', SIGNAL_URL);
video.setAttribute('robot-id', ROBOT_ID);   // last: this attribute set is what starts the session
