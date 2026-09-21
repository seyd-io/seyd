---
title: Robot is behind carrier-grade NAT
description: A cellular or satellite uplink that shares one public address between many customers.
---

:::note[Where this comes from]
This is the page `<seyd-connect-error>` links to for the diagnosis class `robot-cgnat`. The class is chosen by `classify()` in `@seyd/web` from the robot's NAT report and the failure reason; see [Reachability](/docs/networking/reachability/) for how the report is built.
:::

The robot's router got a **private** address from its carrier (usually `100.64.0.0/10`, sometimes `10.0.0.0/8`), so there is no public address on which the robot can be reached, and no port mapping can create one. With no IPv6 either, a direct connection from a browser is impossible from this network.

## What to do, in order of how often it works

1. **Enable IPv6 on the SIM or APN.** Most carriers offer it; many need it switched on in the router's cellular settings or on the APN profile. With a global IPv6 address the robot announces an IPv6 candidate, and the only remaining question is the router's IPv6 firewall (see [Robot IPv6 is firewalled](/docs/networking/classes/robot-ipv6-firewalled/)).
2. **A SIM with a public IPv4 address.** Business and M2M plans usually offer one, sometimes under "static IP" or "public APN". Seyd then port-maps or you forward UDP 4433 once.
3. **Put the robot behind a router that supports PCP or UPnP** on a non-CGNAT uplink, such as a site's fibre or DSL connection.

## What does not help

Port forwarding on the robot's own router. It forwards from an address nobody outside can reach.

## Meanwhile

If the robot allows the relay, the pilot drives through it and the HUD says so. Starlink is a common case of this class; see [Starlink](/docs/networking/starlink/).
