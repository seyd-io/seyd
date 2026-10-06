# Networking: the direct path and the relay

The pilot is a browser, so it is a QUIC client and nothing else: it only
opens outbound connections, and its network needs nothing beyond letting UDP
out. Reachability is one question: **can an unsolicited inbound UDP packet
reach the robot's QUIC port** (4433 by default)? Everything in the plan's
network section follows from the answer for the user's network.

## What the robot announces

At start and on every network change the agent gathers candidates:

| Label | Priority | Where it comes from |
|---|---|---|
| `host` | 240 | Each local IPv4 address. Works only from the robot's own LAN. |
| `portmap` | 220 | The router's external address and port when it granted a PCP, NAT-PMP or UPnP mapping (a 3600 s lease the agent renews) *and* that address is globally routable. A mapping to a private address (double NAT, CGNAT) is not advertised. |
| `host6` | 200 | Each global IPv6 address, when `ipv6 = true`. IPv6 removes the NAT, not the router's firewall. |
| `srflx` | 150 | The public address and port two STUN servers agreed on. Skipped behind a symmetric NAT. |

With them travels a `NatReport` (`ipv4.nat` cone / symmetric / unknown,
`ipv4.cgnat`, `ipv6`, `portmap` result, a `hint` of `likely` / `lan-only` /
`none`). On every announce the cloud's prober makes one real QUIC dial per
public candidate and folds the result in (`candidates[].ok`,
`ipv6.inbound_ok`), so the console and the landing page show the outlook
before any pilot tries. `seydd`'s startup log (`candidate …`, `nat_report`)
says the same on the robot.

## The race in the browser

The engine dials every candidate by priority: `host` and `portmap` at once,
`srflx` and `host6` after a 400 ms hold so the agent's probe packets toward
the pilot can open NAT state first. The first WebTransport session ready
wins and every other attempt is closed. Deadline from the hint: 10 s for
`likely`, 4 s `lan-only`, 2 s `none`. A race ends with one `FailureReason`:
`no-candidates`, `all-candidates-timeout`, `cert-mismatch`,
`handshake-timeout`; `robot-offline` and `token-rejected` come from
signalling before any race; `relay-unavailable` from the relay step.

A page on a public origin is refused a connection to a private address
unless the user allows Chrome's local-network-access prompt; a same-LAN
pilot then falls through `host` to a hairpin over `srflx` or `portmap`.

## The relay decision

Taken last and only when offered: after the race has failed, if the offer
carried a relay (the robot announced `relay = true` and the server has it
enabled) and the session allows it (`relay` option; `relay="0"` on the
element), the engine emits `relay` with the diagnosis and attaches to the
WebSocket relay on the signal server. No relay-first mode, no retry through
the relay, no live upgrade back: a relayed session stays relayed until it
ends, and reload retries the race. It is always shown: `transport:
'relay'`, the amber RELAY badge, the amber HUD path line, the guidance box.
It is metered, priced separately and adds a hop and a TCP leg; it drops on a
server deploy and at the hosted platform's 60-minute request cap (the pilot
re-races and re-relays). Treat it as a first-day fallback while the network
change below is made, never as the design.

## Check before enrolling

On a cellular or unfamiliar uplink, read the router's WAN address first. An
address in `100.64.0.0/10` (or any private range) on the WAN side means
carrier-grade NAT or double NAT: no port mapping can produce a direct path
from there, and the plan must include the SIM, APN or IPv6 change below
before any pilot tries. A globally routable WAN address plus UPnP or
NAT-PMP, or a manual forward of UDP 4433, is the ordinary case.

## What to change, by network (from the field runs)

- **Office or home network with an ordinary (cone) NAT**: works without
  configuration, by hole punch over `srflx`.
- **Consumer 4G/5G router**: not reachable out of the box on either address
  family (carrier NAT port-restricted; the router's stateful IPv6 firewall
  drops unsolicited inbound). **Enable UPnP or NAT-PMP on the router** and
  the agent installs the pinhole itself (`portmap`). Otherwise forward UDP
  4433 to the robot once, or use a SIM with a public IP.
- **Carrier-grade NAT** (`100.64.0.0/10` on the router's WAN side): no port
  mapping can help. Enable IPv6 on the SIM or APN, get a public-IP SIM, or
  put the robot behind a router on a non-CGNAT uplink. Starlink's standard
  plan is CGNAT; its IPv6 is the way out (`docs/starlink.md`).
- **Global IPv6 present**: open a pinhole for UDP 4433 in the router's IPv6
  firewall; the prober's `inbound_ok` says whether it worked.
- **Static public address or data centre**: forward or allow UDP 4433; the
  `srflx` or `portmap` candidate then answers the prober.
- **Pilot side**: only outbound UDP matters. A corporate network or VPN that
  blocks it is fixed by another network; a phone hotspot works.
- **Ports**: the robot needs outbound 443/TCP (signalling WebSocket) and
  inbound UDP on `quic_port`; the pilot needs outbound 443/TCP and outbound
  UDP.

## Failure classes shown to the operator

`<seyd-connect-error>` sorts a failure into one of: `robot-offline`,
`cert-or-token`, `pilot-udp-blocked`, `robot-cgnat`, `robot-symmetric-nat`,
`robot-port-restricted`, `robot-upstream-firewall`, `robot-ipv6-firewalled`,
`unknown`, and links each to its own page in the developer documentation's
Networking section (for example `/docs/networking/classes/robot-cgnat/`)
with the exact router or SIM change. When a user reports "it will not
connect", read the guidance box's Diagnostics (reason, candidates, NAT
report) or the console's robot page before changing anything.
