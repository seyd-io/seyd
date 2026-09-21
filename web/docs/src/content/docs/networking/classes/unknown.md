---
title: No direct connection
description: Every path failed and the report does not point at one cause.
---

:::note[Where this comes from]
This is the page `<seyd-connect-error>` links to for the diagnosis class `unknown`. The class is chosen by `classify()` in `@seyd/web` from the robot's NAT report and the failure reason; see [Reachability](/docs/networking/reachability/) for how the report is built.
:::

All of the robot's candidates timed out and the NAT report does not match a single known pattern. Work through the general checks; the console's robot page shows the NAT report and the prober's per-candidate verdict, which usually narrows it down.

## Checks

1. **Does UDP `<quic_port>` reach the robot from outside?** Enable UPnP, NAT-PMP or PCP on the router (Seyd installs the mapping itself), or forward the port by hand.
2. **Is IPv6 available on both ends?** With a global address on the robot and an open pinhole, IPv6 is often the simplest direct path.
3. **Is the pilot's network blocking UDP?** Try a phone hotspot; if that connects, see [Your network blocks UDP/QUIC](/docs/networking/classes/pilot-udp-blocked/).
4. **Is something in front of the router?** A modem in router mode or a carrier gateway adds a second NAT; see [Upstream firewall or double NAT](/docs/networking/classes/robot-upstream-firewall/).

If the robot allows the relay, the session is carried by it in the meantime and the HUD says so.
