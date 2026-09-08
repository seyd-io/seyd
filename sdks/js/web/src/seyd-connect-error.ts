import { P2pFailure, SeydSession } from '@seyd/core';

export type FailureClass =
  | 'pilot-udp-blocked' | 'robot-cgnat' | 'robot-symmetric-nat' | 'robot-port-restricted'
  | 'robot-upstream-firewall' | 'robot-ipv6-firewalled' | 'cert-or-token' | 'robot-offline' | 'unknown';

export interface Guidance { cls: FailureClass; title: string; body: string; doc: string }

const DOCS = 'https://docs.seyd.io/networking/';

/** Map a failure + NatReport to the mitigation class (PLAN.md §2.6). */
export function classify(f: P2pFailure): Guidance {
  const n = f.natReport ?? {};
  const port = (f.candidates[0]?.url.match(/:(\d+)\//) ?? [])[1] ?? '4433';
  const local = n.ipv4?.local?.[0] ?? '<robot LAN ip>';
  const portmapOk = !!n.portmap?.external && !n.portmap?.error;
  const hasV6 = !!n.ipv6?.present && (n.ipv6?.global?.length ?? 0) > 0;
  const g = (cls: FailureClass, title: string, body: string): Guidance => ({ cls, title, body, doc: DOCS + cls });

  if (f.reason === 'robot-offline') return g('robot-offline', 'Robot is offline', 'The robot is not registered with signaling. Check that the Seyd agent is running and has internet access.');
  // Two very different causes used to share one message that told an operator
  // to restart the agent — useless advice when the real problem is that the
  // robot simply is not public and the viewer has no session token.
  if (f.reason === 'token-rejected') return g('cert-or-token', 'Not authorised for this robot', 'Signaling refused the session token. This robot is not published for public access, or the token has expired — sign in to the console and open the pilot from there.');
  if (f.reason === 'cert-mismatch') return g('cert-or-token', 'Robot certificate mismatch', 'The robot presented a certificate that does not match the one it announced, usually because it rotated its certificate mid-session. Reconnect; if it persists, restart the agent.');
  if (f.reason === 'pilot-udp-blocked') return g('pilot-udp-blocked', 'Your network blocks UDP/QUIC', `Common on corporate Wi-Fi and VPNs. Try another network (a phone hotspot works) or ask IT to allow outbound UDP 443 and ${port}. The robot itself is reachable.`);
  if (n.ipv4?.cgnat && !portmapOk && !hasV6) return g('robot-cgnat', 'Robot is behind carrier-grade NAT', 'A direct connection from a browser is impossible from behind CGNAT with no IPv6. Fixes: enable IPv6 on the SIM/APN (most carriers offer it); use a SIM with a public IP; or put the robot behind a router that supports PCP/UPnP.');
  if (n.ipv4?.nat === 'symmetric' && !portmapOk) return g('robot-symmetric-nat', 'Robot router uses symmetric NAT', `Enable UPnP, NAT-PMP or PCP on the router, or add a manual forward of UDP ${port} → ${local}:${port}.`);
  if (n.ipv4?.nat === 'port-restricted' && n.portmap?.error) return g('robot-port-restricted', 'Port mapping failed on the robot router', `Port mapping failed (${n.portmap.error}). Enable UPnP/NAT-PMP/PCP or forward UDP ${port} → ${local}:${port}.`);
  if (portmapOk && (n.prober?.unreachable?.includes('portmap') || f.reason === 'all-candidates-timeout')) return g('robot-upstream-firewall', 'Upstream firewall or double NAT', `The router accepted a port mapping (${n.portmap?.external}) but it cannot be reached — usually a second NAT layer or the ISP modem's firewall. Forward UDP ${port} on the outer router too.`);
  if (hasV6 && n.ipv6?.inbound_ok === false) return g('robot-ipv6-firewalled', 'Robot IPv6 is firewalled', `The robot has a global IPv6 address but inbound UDP ${port} is blocked. Open a pinhole for UDP ${port} on the router's IPv6 firewall.`);
  return g('unknown', 'No direct connection', `All ${f.candidates.length} path(s) failed (${f.reason}). Check that UDP ${port} reaches the robot: enable UPnP/PCP, forward the port, or enable IPv6 on both ends.`);
}

/** <seyd-connect-error> — shows mitigation guidance for a P2P failure. Set `.session` or `.failure`. */
export class SeydConnectErrorElement extends HTMLElement {
  private box: HTMLDivElement;
  private unsub: (() => void)[] = [];
  private _session: SeydSession | null = null;

  constructor() {
    super();
    const root = this.attachShadow({ mode: 'open' });
    root.innerHTML = `<style>
      :host { display: block; font: 14px/1.45 system-ui, sans-serif; color: #eee; }
      .box { background: #3a1c1c; border: 1px solid #a33; border-radius: 8px; padding: 12px 16px; max-width: 560px; }
      .box[hidden] { display: none; } h3 { margin: 0 0 6px; font-size: 15px; } p { margin: 0 0 8px; } a { color: #9cf; }
      details { font-size: 12px; opacity: .8 } pre { white-space: pre-wrap; margin: 4px 0 0; }
    </style><div class="box" hidden></div>`;
    this.box = root.querySelector('.box')!;
  }

  set session(s: SeydSession | null) {
    this.unsub.forEach((u) => u()); this.unsub = [];
    this._session = s;
    if (!s) return;
    this.unsub.push(s.on('p2p-failed', (f) => { this.failure = f; }));
    this.unsub.push(s.on('state', ({ state }) => { if (state === 'connected') this.failure = null; }));
  }
  get session(): SeydSession | null { return this._session; }

  set failure(f: P2pFailure | null) {
    if (!f) { this.box.hidden = true; return; }
    const g = classify(f);
    this.box.hidden = false;
    this.box.innerHTML = `<h3>${g.title}</h3><p>${g.body}</p>` +
      `<p><a href="${g.doc}" target="_blank" rel="noopener">How to fix this →</a> <span style="opacity:.6">(${g.cls})</span></p>` +
      `<details><summary>Diagnostics</summary><pre>${escapeHtml(JSON.stringify({ reason: f.reason, detail: f.detail, candidates: f.candidates.map((c) => c.label + ' ' + c.url), nat_report: f.natReport }, null, 1))}</pre></details>`;
  }
}

function escapeHtml(s: string): string { return s.replace(/[&<>]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;' }[c]!)); }

if (!customElements.get('seyd-connect-error')) customElements.define('seyd-connect-error', SeydConnectErrorElement);
