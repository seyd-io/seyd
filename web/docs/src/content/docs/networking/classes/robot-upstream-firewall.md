---
title: Upstream firewall or double NAT
description: The router accepted a port mapping, but the mapped address cannot be reached from outside.
---

:::note[Where this comes from]
This is the page `<seyd-connect-error>` links to for the diagnosis class `robot-upstream-firewall`. The class is chosen by `classify()` in `@seyd/web` from the robot's NAT report and the failure reason; see [Reachability](/docs/networking/reachability/) for how the report is built.
:::

Seyd installed a port mapping and announced it, and the cloud's prober still could not reach the robot on it. That combination means something in front of the router is dropping the traffic: a second NAT layer (an ISP modem in router mode, a carrier's gateway), or a firewall on the site's edge.

## What to do

1. **Find the outer device.** If the router's WAN address is private (`10.x`, `192.168.x`, `172.16–31.x`, `100.64–127.x`), there is another router in front of it.
2. **Forward UDP `<quic_port>` on the outer router too**, to the inner router's WAN address; or put the modem in bridge mode so there is only one NAT.
3. **On a managed site network**, ask for an inbound rule for UDP `<quic_port>` to the robot's address. Nothing else is needed; the robot makes all other connections outbound.

If the outer address is carrier-grade NAT, this becomes the [CGNAT](/docs/networking/classes/robot-cgnat/) case.
