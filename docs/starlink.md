# Seyd over Starlink

Best-judgment assessment (2026-08-31), grounded in field runs A/B and current
public information about Starlink's network. Not yet tested on real hardware.

## The question that decides everything

Same as field run B: **can an unsolicited inbound packet reach the robot's QUIC
port (UDP 4433)?** The pilot is a browser, i.e. a QUIC *client*, so only the
*robot* must be reachable; the pilot's own network only makes outbound
connections and never needs to be open. So "both ends on Starlink" is no harder
than "robot on Starlink" — it comes down entirely to the robot's inbound
reachability.

## What Starlink gives you

| | Residential / Roam / Mobile | Priority / Business (incl. Mobile Priority) |
|---|---|---|
| IPv4 | **CGNAT**, no public IPv4, no inbound | **Public IPv4 available as an add-on** (~$140+/mo); removes CGNAT |
| IPv6 | **Global, routable /56** via DHCPv6-PD — every device gets a public address, no NAT | Same |
| Stock router (Gen 2 / Gen 3 / Business) | **No port forwarding, no IPv6 firewall pinhole — on any tier** | Same |

Two facts from this that matter most:

1. **No Starlink-issued router supports the inbound port-forward / IPv6 pinhole
   we need — on any plan.** The tiers differ in *addressing* (the public-IPv4
   add-on), not in router capability. So the answer to "does a higher tier come
   with a router that has the support we need" is **no**; the differentiator is
   the public-IP add-on, plus what you put behind the dish.
2. **IPv6 is native and global on every tier, including residential.** That is
   the cheap escape hatch — but IPv6 removes the NAT, not the firewall, and the
   stock router will not open an inbound hole. As run B proved, an inbound
   pinhole is still required.

## Two ways to make a Seyd robot on Starlink reachable

**Path A — IPv6, any plan (cheapest).** Put the Starlink router in **Bypass
Mode** and run your own router (UniFi, MikroTik, pfSense, Firewalla) that
requests the /56 prefix delegation and adds an **inbound IPv6 firewall rule for
UDP 4433** to the robot. Works on plain Residential because the global /56 is
universal. Cost: a ~$100–200 router, no plan upgrade. **Catch:** the *pilot*
must also have IPv6 (a browser on an IPv4-only network cannot use a v6
candidate). Seyd already advertises the robot as a `host6` candidate; this is
exactly the path that lit up behind the 4G router in run B once the firewall let
it in.

**Bypass Mode is an official, supported Starlink feature** — a toggle in the
Starlink app that turns the Starlink router into a pass-through (Wi-Fi and
routing off) so your own router does the work. It is reversible and not a hack.
Hardware caveat by dish generation: the round **Gen 2** dish has no LAN port of
its own, so bypass mode needs the official **Starlink Ethernet Adapter** (~$25)
to connect your router; the **Gen 3** dish (and the Business/Flat High
Performance units) has Ethernet built in, so no adapter is required.

**Path B — public IPv4, works for any pilot.** Take a **Priority / Business**
plan with the **public-IP add-on** (~$140+/mo). That gives a normal public IPv4;
forward UDP 4433 to the robot (still via bypass mode + your own router, or via
whatever inbound the public-IP config allows). This reaches IPv4-only pilots
too, so it is the choice when you don't control the pilot's network.

**Neither available → relay tier.** A stock-router residential Starlink with an
IPv4-only pilot is genuinely unreachable directly — a relay-tier customer.

## Will the link quality hold?

Yes, and Starlink plays to Seyd's strengths. LEO base latency is ~20–50 ms one
way (fine for teleoperation), but satellite handovers every ~15 s cause brief
latency spikes and **bursty loss** — the same profile we exercised on cellular
in runs A/B. The mitigations are all in: per-block FEC recovers without a round
trip, the adaptive close-out deadline turns handover jitter into patience rather
than fake loss, the ABR raises parity to 50 % under loss and trims bitrate on
RTT inflation, and keyframe-on-loss caps a smear at one round trip. Residual
risk: a handover burst longer than the delta parity shows a brief red border and
a fast recover — the honest behaviour. One real gap: **re-gathering candidates
on a network change is still a stub**, and a Starlink prefix/IP can change, so a
long-lived robot needs that (already on the roadmap), plus port-mapping renewal.

## Bottom line

| Robot placement | Pilot | Direct P2P? |
|---|---|---|
| Starlink, stock router | any | No → relay |
| Starlink, **bypass + own router, IPv6 pinhole**, any plan | **has IPv6** | **Yes (`host6`)** |
| Starlink, **Priority/Business public-IPv4** + forward 4433 | any | **Yes (`portmap`/`host`)** |
| Both ends on different Starlinks | — | **Same as robot-only** (pilot side is client-only) |

**Confidence:** high — residential global /56 IPv6 and the stock-router
limitation are consistently confirmed across multiple 2026 sources. The **cloud
prober** (the `inbound_ok` measurement done by hand in run B) is what would turn
this into a per-install answer at enrolment instead of at the first session.

Sources: Starlink Help Center "What IP address does Starlink provide?";
satspeedcheck.com CGNAT-bypass guide (2026); Llama Networks "Starlink IP
Addressing"; CellStream "Does Starlink support IPv6?"; packetville.net
self-hosting-behind-Starlink-with-IPv6.
