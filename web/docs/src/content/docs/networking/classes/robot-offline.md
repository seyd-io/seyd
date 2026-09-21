---
title: Robot is offline
description: The cloud has no live signalling connection from this robot.
---

:::note[Where this comes from]
This is the page `<seyd-connect-error>` links to for the diagnosis class `robot-offline`. The class is chosen by `classify()` in `@seyd/web` from the robot's NAT report and the failure reason; see [Reachability](/docs/networking/reachability/) for how the report is built.
:::

The robot id exists, but nothing is currently announced under it: the daemon is not running, has no internet access, or was refused by signalling.

## What to do

1. **Check that the agent is running** on the robot (`systemctl status seydd`, or the process you started) and read its log.
2. **Look for `denied` in the log.** `unknown-robot` means the robot was never enrolled; redeem an enrolment token (see [Enrolment and access](/docs/cloud/access/)). `key-mismatch` means the id is enrolled with a different key: another agent is using the id, or the credential file was replaced. Delete the robot in the console and enrol again.
3. **After a cloud deploy**, a robot can show offline for a while: its signalling WebSocket stays on the draining old revision until it drops. Restarting the agent fixes it at once.

The console's fleet page shows *last seen* for an offline robot, which usually says which of these it is.
