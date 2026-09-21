---
title: Robot router uses symmetric NAT
description: The router rewrites the source port per destination, so the address the robot learned is not the one your browser can use.
---

:::note[Where this comes from]
This is the page `<seyd-connect-error>` links to for the diagnosis class `robot-symmetric-nat`. The class is chosen by `classify()` in `@seyd/web` from the robot's NAT report and the failure reason; see [Reachability](/docs/networking/reachability/) for how the report is built.
:::

Seyd's STUN probe found that the robot's router allocates a different external port for every destination. The public address the robot announces is only valid for the cloud that helped it discover it, so a browser connecting from anywhere else hits a closed port. Port mapping was not available to fix it.

## What to do

1. **Enable UPnP, NAT-PMP or PCP on the router.** Seyd asks the router for a mapping itself on every start and network change, installs the pinhole and announces the mapped address as a `portmap` candidate. On most consumer and 4G routers this is one checkbox. This is the fix we recommend, and the one field testing settled on for robots behind 4G routers.
2. **Or forward the port once by hand:** UDP `<quic_port>` (4433 by default) on the router to the robot's LAN address and the same port. Give the robot a DHCP reservation so the address does not move.

After either change, restart the daemon or wait for its next re-announce; the console shows the new candidate and the prober's verdict before anyone connects.
