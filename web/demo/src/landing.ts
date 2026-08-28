// Landing page: live list of robots from the signal server's console presence
// feed (docs/protocol/signal-v2.md), with a REST fallback. Old links of the
// form /?robot=<id> are forwarded to the pilot page.

const params = new URLSearchParams(location.search);
if (params.get('robot')) {
  location.replace(`./pilot/${location.search}`);
}

const wsProto = location.protocol === 'https:' ? 'wss' : 'ws';
const SIGNAL_URL = params.get('signal') ||
  (location.hostname === 'localhost' || location.hostname === '127.0.0.1' ? 'ws://localhost:8080/ws' : `${wsProto}://${location.host}/ws`);
const signalHttp = SIGNAL_URL.replace(/^ws/, 'http').replace(/\/ws$/, '');
const listEl = document.getElementById('robot-list')!;
const signalText = document.getElementById('signal-text')!;

interface SessionInfo { session_id: string; role: string; subject: string }
interface Robot {
  robot_id: string; online: boolean; channels?: { id: number; kind: string; name: string; codec: string }[];
  p2p_hint?: string | null; sessions?: SessionInfo[]; status?: unknown; last_seen?: string | null;
}

let ws: WebSocket | null = null;
let retry = 2000;

function connect(): void {
  ws = new WebSocket(SIGNAL_URL);
  ws.onopen = () => {
    retry = 2000;
    signalText.textContent = 'live';
    ws!.send(JSON.stringify({ type: 'auth', v: 2, role: 'console' }));
  };
  ws.onmessage = (e) => {
    const m = JSON.parse(e.data as string);
    if (m.type === 'auth-ok') ws!.send(JSON.stringify({ type: 'subscribe-presence' }));
    else if (m.type === 'presence') render(m.robots as Robot[]);
    else if (m.type === 'denied') { signalText.textContent = `denied: ${m.reason}`; fallback(); }
  };
  ws.onclose = () => { signalText.textContent = 'reconnecting…'; setTimeout(connect, retry); retry = Math.min(retry * 2, 15000); };
  ws.onerror = () => { signalText.textContent = 'error'; fallback(); };
}

async function fallback(): Promise<void> {
  try {
    const r = await fetch(`${signalHttp}/api/v1/robots`);
    if (r.ok) render(((await r.json()) as { robots: Robot[] }).robots);
  } catch { /* the WebSocket retry will try again */ }
}

function esc(s: unknown): string {
  return String(s).replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]!));
}

function render(robots: Robot[]): void {
  if (!robots.length) { listEl.innerHTML = '<div class="empty">No robots registered.</div>'; return; }
  const sorted = [...robots].sort((a, b) => Number(b.online) - Number(a.online) || a.robot_id.localeCompare(b.robot_id));
  listEl.innerHTML = sorted.map((r) => {
    const sessions = r.sessions ?? [];
    const driver = sessions.find((s) => s.role === 'driver');
    const observers = sessions.filter((s) => s.role === 'observer').length;
    const dot = !r.online ? '' : driver ? 'in-use' : 'available';
    const kinds = (r.channels ?? []).map((c) => `${c.kind}:${c.name}`).join(' · ');
    const status = !r.online
      ? `offline${r.last_seen ? ' · last seen ' + new Date(r.last_seen).toLocaleTimeString() : ''}`
      : `${driver ? 'driver: ' + esc(driver.subject) : 'available'}${observers ? ` · ${observers} observer${observers > 1 ? 's' : ''}` : ''} · p2p ${esc(r.p2p_hint ?? '?')}`;
    const href = `./pilot/?robot=${encodeURIComponent(r.robot_id)}${params.get('signal') ? '&signal=' + encodeURIComponent(SIGNAL_URL) : ''}`;
    return `<div class="robot-item">
      <div class="robot-left"><div class="dot ${dot}"></div>
        <div class="robot-meta"><div class="robot-id">${esc(r.robot_id)}</div><div class="robot-status">${status}</div><div class="robot-status">${esc(kinds)}</div></div></div>
      <a class="connect-btn ${r.online ? '' : 'disabled'}" href="${href}">${driver ? 'Observe →' : 'Connect →'}</a>
    </div>`;
  }).join('');
}

connect();
