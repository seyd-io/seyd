import '@seyd/web';
import { mountThemeSwitch } from '@seyd/theme';
import type { SeydSession } from '@seyd/core';
import type { SeydConnectErrorElement, SeydHudElement, SeydVideoElement } from '@seyd/web';
import { PtzController } from './ptz.js';
import { FlightController, formatTelemetry } from './flight.js';

const params = new URLSearchParams(location.search);
const ROBOT_ID = params.get('robot') || 'seyd-demo';
// A short-lived ES256 session token, minted by the console's "open pilot" or
// by a customer's own backend. Absent on the public demo, where `seyd-demo`
// carries a public grant instead.
const TOKEN = params.get('token');
const SIGNAL_URL = params.get('signal') || (location.hostname === 'localhost' || location.hostname === '127.0.0.1' ? 'ws://localhost:8080/ws' : `${location.protocol === 'https:' ? 'wss' : 'ws'}://${location.host}/ws`);
const QOS_PROFILES = ['latency', 'balanced', 'quality'];
let qos = params.get('qos') || localStorage.getItem('seyd.qos') || 'balanced';
if (!QOS_PROFILES.includes(qos)) qos = 'balanced';

const video = document.getElementById('video') as SeydVideoElement;
const hud = document.getElementById('hud') as SeydHudElement;
const err = document.getElementById('err') as SeydConnectErrorElement;
const qosSelect = document.getElementById('qos') as HTMLSelectElement;
const hudBtn = document.getElementById('hud-btn') as HTMLButtonElement;
const hintEl = document.getElementById('hint')!;
const sensorEl = document.getElementById('sensor')!;
const roleEl = document.getElementById('role')!;
const lanHintEl = document.getElementById('lan-hint')!;
const noticeEl = document.getElementById('notice')!;
const takeoffBtn = document.getElementById('takeoff-btn') as HTMLButtonElement;
const landBtn = document.getElementById('land-btn') as HTMLButtonElement;
document.getElementById('robot')!.textContent = ROBOT_ID;
qosSelect.value = qos;
mountThemeSwitch(document.getElementById('theme-switch')!);

let ptz: PtzController | null = null;
let flight: FlightController | null = null;

video.addEventListener('seyd-session', (e) => {
  const session = (e as CustomEvent<SeydSession>).detail;
  hud.session = session;
  err.session = session;
  ptz?.dispose();
  ptz = new PtzController(session, video.canvas, video);
  flight?.dispose();
  flight = new FlightController(session, video.canvas, video);
  session.on('welcome', ({ role, pathLabel }) => {
    const hasPtz = session.hasCommandChannel('ptz');
    // A robot declares what it can be told: a `ptz` channel gets the camera
    // controls, a `flight` channel the drone's. Both are velocity schemes with
    // a hold on the robot, so an abandoned gesture stops on its own.
    const hasFlight = session.hasCommandChannel('flight');
    ptz?.setEnabled(hasPtz && role === 'driver');
    flight?.setEnabled(hasFlight && role === 'driver');
    takeoffBtn.hidden = landBtn.hidden = !(hasFlight && role === 'driver');
    noticeEl.textContent = hasFlight
      ? 'You are flying a physical drone. L or the land button lands it; it lands itself when you leave.'
      : 'You are controlling a physical camera. Response latency reflects your connection.';
    roleEl.textContent = role === 'driver' ? 'driver' : 'observer — someone else is driving; controls disabled';
    // Driver in the accent (you hold the direct path); observer in amber (someone else does).
    roleEl.className = role === 'driver' ? 'tag on' : 'tag warn';
    roleEl.hidden = false;
    // Chrome refuses a public-origin page a direct connection to a private IP
    // (Local Network Access) unless the user allows it; the race then falls
    // through to the hairpin. Only meaningful when a LAN candidate was offered.
    const hadHost = session.candidates.some((c) => c.label === 'host');
    const viaHairpin = pathLabel === 'srflx' || pathLabel === 'portmap';
    lanHintEl.textContent = 'Chrome blocked the direct LAN connection (local network access); if you are on the robot\'s network, allow it in the site permissions for lowest latency.';
    lanHintEl.hidden = !(hadHost && viaHairpin && location.protocol === 'https:');
    hintEl.textContent = (hasPtz ? 'DRAG or ARROWS — look · SHIFT — fast · WHEEL or +/− — zoom · H — home · ' : '')
      + (hasFlight ? 'T — take off · L — land · ARROWS or DRAG — move · R/F — up/down · Q/E — turn · SHIFT — fast · ' : '')
      + 'SPACE — snapshot · S — stats';
  });
  session.on('stats', (s) => { const w = window as unknown as { __seydTrace?: string[] }; if (w.__seydTrace && s.trace) w.__seydTrace.push(...s.trace); });
  session.on('sensor', ({ channel, data }) => {
    const flightLine = formatTelemetry(data);
    sensorEl.textContent = flightLine ? `${channel.name}: ${flightLine}` : `${channel.name}: ${typeof data === 'string' ? data : JSON.stringify(data)}`;
  });
  session.on('state', ({ state }) => {
    if (state !== 'connected') { ptz?.setEnabled(false); flight?.setEnabled(false); roleEl.hidden = true; lanHintEl.hidden = true; takeoffBtn.hidden = landBtn.hidden = true; }
  });
});

// The HUD toggles with `S`, which a phone or a touch panel does not have, so
// the same toggle is a button. The pressed state mirrors the HUD's own
// visibility, which it persists in localStorage across reloads.
const syncHudBtn = () => hudBtn.setAttribute('aria-pressed', String(hud.visible));
hudBtn.addEventListener('click', () => { hud.toggle(); syncHudBtn(); });
document.addEventListener('keydown', (e) => { if (e.code === 'KeyS') queueMicrotask(syncHudBtn); });
syncHudBtn();
takeoffBtn.addEventListener('click', () => flight?.takeoff());
landBtn.addEventListener('click', () => flight?.land());

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
// ?relay=0 refuses the cloud relay (ADR 0010), for diagnosing the direct path.
if (params.get('relay') !== null) video.setAttribute('relay', params.get('relay')!);
if (params.get('trace')) { video.setAttribute('trace', '1'); (window as unknown as { __seydTrace: string[] }).__seydTrace = []; }
// ?pd=0 restores decode-on-arrival, for A/B against the paced default.
if (params.get('pd') !== null) video.setAttribute('presentation-delay', params.get('pd')!);
if (loss > 0) { video.setAttribute('loss', String(loss)); video.setAttribute('burst', params.get('burst') ?? '1'); }
video.setAttribute('qos', qos);

/**
 * Every pilot needs a session token; the signal server accepts none without
 * one. A link from the console carries `?token=`, but a visitor arriving at
 * the public demo has nothing, so the page asks for one. The server grants it
 * only if the robot carries a public grant — which is what replaced the old
 * `SEYD_DEV_ALLOW_ANONYMOUS`, and is why this request is unauthenticated.
 *
 * Tries `drive` first and falls back to `observe`, so a robot published for
 * viewing only still works instead of failing shut.
 */
async function obtainToken(): Promise<string | null> {
  if (TOKEN) return TOKEN;
  const api = SIGNAL_URL.replace(/^ws/, 'http').replace(/\/ws$/, '');
  for (const scope of ['drive', 'observe']) {
    try {
      const res = await fetch(`${api}/api/v1/session-tokens`, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ robot_id: ROBOT_ID, scope }),
      });
      if (res.ok) return (await res.json()).token as string;
      if (res.status !== 401 && res.status !== 403) break;   // not a permissions problem
    } catch {
      break;   // offline or blocked; fall through to connecting without one
    }
  }
  return null;
}

void obtainToken().then((token) => {
  if (token) video.setAttribute('token', token);
  video.setAttribute('signal-url', SIGNAL_URL);
  video.setAttribute('robot-id', ROBOT_ID);   // last: this attribute set is what starts the session
});
