---
title: Not authorised, or a certificate mismatch
description: Signalling refused the session, or the robot presented a certificate other than the one it announced.
---

:::note[Where this comes from]
This is the page `<seyd-connect-error>` links to for the diagnosis class `cert-or-token`. The class is chosen by `classify()` in `@seyd/web` from the robot's NAT report and the failure reason; see [Reachability](/docs/networking/reachability/) for how the report is built.
:::

Two causes share this page because both are about identity rather than the network.

## Not authorised for this robot

Signalling refused the session token. Either the robot is not published for public access and the pilot page carried no token, or the token has expired (a session pass lasts five minutes). Sign in to the console and open the pilot from the robot's page, which mints a fresh pass; or, for your own application, have your backend mint one with `POST /api/v1/sessions` for a signed-in operator. See [Enrolment and access](/docs/cloud/access/).

## Robot certificate mismatch

The robot's QUIC certificate did not match the fingerprint in the offer, which happens when the agent rotated its certificate between the announce and the connect. Reconnect; the next offer carries the new fingerprint. If it keeps happening, restart the agent and check the robot's clock, since rotation is time-based.
