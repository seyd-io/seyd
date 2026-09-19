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

/**
 * <seyd-connect-error> — shows mitigation guidance for a P2P failure. Set
 * `.session` or `.failure`. While a session is carried by the cloud relay
 * (ADR 0010) the same guidance is available in amber under a "relayed"
 * heading, collapsed to one line by default so it never sits in the way of
 * driving: the picture works, but the network fix is still worth making.
 * Both forms can be dismissed with the × (or Escape); the next failure or
 * relay brings the box back.
 */
export class SeydConnectErrorElement extends HTMLElement {
  private box: HTMLDivElement;
  private unsub: (() => void)[] = [];
  private _session: SeydSession | null = null;
  private current: { f: P2pFailure; relayed: boolean } | null = null;
  private expanded = false;

  constructor() {
    super();
    const root = this.attachShadow({ mode: 'open' });
    root.innerHTML = `<style>
      /* Over the picture: scrim tokens, so the box reads the same in both themes.
         Red border = no session; amber border = relayed (docs/design.md). */
      :host { display: block; font: 13.5px/1.45 var(--seyd-font-body, "IBM Plex Sans", system-ui, sans-serif); color: var(--seyd-on-scrim, #e7eeec); }
      .box {
        position: relative; max-width: 560px; padding: 12px 36px 12px 16px; border-radius: var(--seyd-radius, 6px);
        background: var(--seyd-scrim, rgba(8,14,15,.72)); border: 1px solid var(--seyd-danger, #e0776c); border-left-width: 3px;
        backdrop-filter: blur(6px);
      }
      .box.relayed { border-color: var(--seyd-amber, #d9a441); }
      .box.relayed.compact { padding: 6px 36px 6px 12px; font-size: 12.5px; white-space: nowrap; }
      .box[hidden] { display: none; }
      h3 { margin: 0 0 6px; font: 600 14px/1.3 var(--seyd-font-display, "Familjen Grotesk", system-ui, sans-serif); letter-spacing: -.005em; }
      .relayed h3 { color: var(--seyd-amber, #d9a441); }
      p { margin: 0 0 8px; color: var(--seyd-on-scrim-2, #b4c2bf); }
      a { color: var(--seyd-accent, #12a37a); }
      details { font-size: 11.5px; opacity: .8; font-family: var(--seyd-font-mono, ui-monospace, monospace); } pre { white-space: pre-wrap; margin: 4px 0 0; }
      button { font: inherit; color: inherit; background: none; border: 0; cursor: pointer; padding: 0; }
      .close { position: absolute; top: 6px; right: 8px; font-size: 18px; line-height: 1; opacity: .7; padding: 2px 6px; }
      .close:hover { opacity: 1; }
      .why { text-decoration: underline; color: var(--seyd-amber, #d9a441); margin-left: 8px; }
    </style><div class="box" hidden></div>`;
    this.box = root.querySelector('.box')!;
    this.box.addEventListener('click', (e) => {
      const t = e.target as HTMLElement;
      if (t.closest('.close')) { this.dismiss(); return; }
      if (t.closest('.why')) { this.expanded = !this.expanded; this.render(); }
    });
    // Drags that start on the box must never reach the video surface as
    // camera input, and the box must never swallow the keyboard.
    this.box.addEventListener('pointerdown', (e) => e.stopPropagation());
    this.box.addEventListener('keydown', (e) => { if (e.key === 'Escape') this.dismiss(); });
  }

  set session(s: SeydSession | null) {
    this.unsub.forEach((u) => u()); this.unsub = [];
    this._session = s;
    if (!s) return;
    this.unsub.push(s.on('p2p-failed', (f) => { this.show(f, false); }));
    this.unsub.push(s.on('relay', ({ failure }) => { this.show(failure, true); }));
    this.unsub.push(s.on('state', ({ state }) => {
      // Connected directly: nothing to fix. Connected through the relay: the
      // diagnosis stays (collapsed), because the relay is the symptom, not the cure.
      if (state === 'connected' && s.transport !== 'relay') this.failure = null;
    }));
  }
  get session(): SeydSession | null { return this._session; }

  set failure(f: P2pFailure | null) { this.show(f, false); }

  /** Hide until the next failure or relay event. */
  dismiss(): void { this.box.hidden = true; }

  private show(f: P2pFailure | null, relayed: boolean): void {
    if (!f) { this.current = null; this.box.hidden = true; return; }
    this.current = { f, relayed };
    this.expanded = !relayed;
    this.render();
  }

  private render(): void {
    const cur = this.current;
    if (!cur) return;
    const { f, relayed } = cur;
    const g = classify(f);
    this.box.hidden = false;
    this.box.classList.toggle('relayed', relayed);
    this.box.classList.toggle('compact', relayed && !this.expanded);
    const close = '<button class="close" type="button" title="Dismiss" aria-label="Dismiss">×</button>';
    if (relayed && !this.expanded) {
      this.box.innerHTML = `Relayed via Seyd cloud — ${escapeHtml(g.title.charAt(0).toLowerCase() + g.title.slice(1))}.`
        + `<button class="why" type="button">why / how to fix</button>${close}`;
      return;
    }
    const title = relayed ? `Relayed through the Seyd cloud — ${g.title.charAt(0).toLowerCase()}${g.title.slice(1)}` : g.title;
    const lead = relayed ? '<p>No direct path connected, so this session is carried by the cloud relay: it works, with higher latency, and is metered. To go direct: </p>' : '';
    this.box.innerHTML = `${close}<h3>${title}</h3>${lead}<p>${g.body}</p>` +
      `<p><a href="${g.doc}" target="_blank" rel="noopener">How to fix this →</a> <span style="opacity:.6">(${g.cls})</span>` +
      (relayed ? ' <button class="why" type="button">less</button>' : '') + '</p>' +
      `<details><summary>Diagnostics</summary><pre>${escapeHtml(JSON.stringify({ reason: f.reason, detail: f.detail, candidates: f.candidates.map((c) => c.label + ' ' + c.url), nat_report: f.natReport }, null, 1))}</pre></details>`;
  }
}

function escapeHtml(s: string): string { return s.replace(/[&<>]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;' }[c]!)); }

if (!customElements.get('seyd-connect-error')) customElements.define('seyd-connect-error', SeydConnectErrorElement);
