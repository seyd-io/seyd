---
title: Robot IPv6 is firewalled
description: The robot has a global IPv6 address, and inbound UDP to it is blocked.
---

:::note[Where this comes from]
This is the page `<seyd-connect-error>` links to for the diagnosis class `robot-ipv6-firewalled`. The class is chosen by `classify()` in `@seyd/web` from the robot's NAT report and the failure reason; see [Reachability](/docs/networking/reachability/) for how the report is built.
:::

IPv6 removes the NAT but not the firewall. Consumer and 4G routers ship with a stateful IPv6 firewall that allows replies to connections the robot opened and drops everything unsolicited, and a browser's QUIC handshake is unsolicited inbound. Field testing found exactly this on a 4G router: the robot's IPv6 address was global and correct, and every handshake from outside was dropped.

## What to do

1. **Open a pinhole** for UDP `<quic_port>` (4433 by default) to the robot's IPv6 address in the router's IPv6 firewall settings. Some routers call it "IPv6 port opening" or "inbound rules".
2. **Or enable UPnP/PCP**, which on routers that support PCP for IPv6 installs the pinhole for you.
3. **Give the robot a stable address.** Disable privacy extensions on the robot's interface, or the address the rule names will rotate.

The IPv4 candidates may still work if the router allows a port mapping there; Seyd races every reachable candidate and keeps the fastest.
