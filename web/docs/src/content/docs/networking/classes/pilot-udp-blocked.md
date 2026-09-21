---
title: Your network blocks UDP/QUIC
description: The robot is reachable; the pilot's own network is the problem.
---

:::note[Where this comes from]
This is the page `<seyd-connect-error>` links to for the diagnosis class `pilot-udp-blocked`. The class is chosen by `classify()` in `@seyd/web` from the robot's NAT report and the failure reason; see [Reachability](/docs/networking/reachability/) for how the report is built.
:::

The diagnosis is on the **pilot's** side: the robot announced usable candidates and the cloud's prober reached it, but every QUIC handshake from your browser timed out before the first packet. Corporate Wi-Fi, guest networks and most VPN clients block outbound UDP, and WebTransport is QUIC over UDP. The robot itself is fine.

## What to do

1. **Try another network.** A phone hotspot is the fastest test. If the session connects there, the diagnosis is confirmed.
2. **Ask IT to allow outbound UDP** to port 443 and to the robot's QUIC port (shown in the message, 4433 by default). Nothing inbound is needed on the pilot's side.
3. **Disconnect the VPN** for the session, or ask for a split-tunnel rule that excludes the robot's address.

## If none of that is possible

The session can be carried by the cloud relay, which runs over a WebSocket on TCP 443 and passes almost every corporate firewall. The robot must allow it (`relay = true` in `seydd.toml`, the default), and the pilot shows a **relayed** badge and this same guidance in amber for as long as the session is relayed. The relay adds latency and is metered; treat it as the way to drive today, not the way to leave the network.
