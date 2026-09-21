---
title: Port mapping failed on the robot router
description: The NAT type is workable, but the router refused or failed the mapping request.
---

:::note[Where this comes from]
This is the page `<seyd-connect-error>` links to for the diagnosis class `robot-port-restricted`. The class is chosen by `classify()` in `@seyd/web` from the robot's NAT report and the failure reason; see [Reachability](/docs/networking/reachability/) for how the report is built.
:::

The robot's router is a port-restricted NAT, which a mapped port would make reachable, and Seyd asked it for one over PCP, NAT-PMP and UPnP in turn. The request failed; the error is in the message (a refusal, a timeout, or a router that answers but never installs the rule).

## What to do

1. **Turn UPnP or NAT-PMP on** in the router's settings; some ship with it off, some call it "automatic port forwarding".
2. **If the router has no such setting, forward the port by hand:** UDP `<quic_port>` (4433 by default) to the robot's LAN address and port, with a DHCP reservation for the robot.
3. **Check for a second router.** A modem in router mode in front of the Wi-Fi router means the mapping lands on the inner one only; either bridge the modem or forward the port on both.

The console shows the port-mapping result on the robot's page (`portmap` in the NAT report), so you can confirm the change without driving.
