# Enrolment, access and the cloud

Three identity planes; only one touches an identity provider.

| Plane | Who | Mechanism |
|---|---|---|
| Robot identity | The robot | An Ed25519 key generated on the robot, registered once by enrolment, proven by challenge-response on every connection |
| Session authorization | A pilot in one session | An ES256 JWT minted by the cloud with Seyd's own key, naming one robot, one scope, one subject; valid at most 300 s; verified by the robot against `/.well-known/seyd-session-jwks.json` |
| Human identity | A person in the console | OIDC sign-in through the configured provider (self-hosted Logto by default), behind one seam in the cloud |

Robots and sessions never depend on the provider: a fleet keeps working
while it is down. Who may drive which robot lives in Seyd's own database.

## Enrolling a robot

1. An admin creates a one-time enrolment token: console → Fleet → *Create
   enrolment token*, or `node dist/bootstrap.js enrolment-token <org-id>
   "<label>"` in the cloud's API directory on a machine with database access
   (a self-hosted cloud, or the operator of the hosted one).
2. The robot redeems it (`robot-daemon.md`, "Enrolment"): the daemon on
   first start with `SEYD_ENROLMENT_TOKEN`, or `seydd enrol --token …`.
   SDK robots enrol their key file through `seydd enrol` too
   (`robot-sdk.md`).
3. The key file at `credential_path` is now the robot's identity. Back it
   up; never copy it to another robot. Lose it and you need a new token.

Denials: `unknown-robot` (never enrolled; redeem a token), `key-mismatch`
(the id is enrolled with another key; delete the robot in the console and
enrol again). In development only, `SEYD_DEV_OPEN_ENROLMENT=1` on a
self-hosted signal server trusts unknown robots on first connection.

## Who may observe or drive: roles and grants

Every robot belongs to one organisation. Roles are a closed list: `viewer`
(read), `operator` (plus create sessions), `admin` (plus manage members,
robots, enrolment, grants, API keys, audit), `owner` (plus delete the org).
A role alone never lets anyone drive: per-robot permission is a **grant**
with scope `observe` or `drive`, and a person needs both the role and the
grant. A **public grant** has no holder: anyone, with or without an account,
may open a session at that scope (the demo robot; never a production
robot). `GET /api/v1/robots` lists only public-grant robots.

## How a pilot gets a session token

`POST /api/v1/session-tokens` with `robot_id`, optional `scope` (`drive`
default, or `observe`) and `ttl_sec` (≤ 300) returns `{ token, subject,
scope, expires_in, issuer }`. Three callers, which are the three access
models to choose between in the plan:

1. **The user's own backend with an API key** (the OEM path, and the usual
   answer for a product with its own users): the backend authenticates its
   users however it already does, holds a Seyd API key (`seyd_live_…`,
   created on the console's Keys page or `POST /api/v1/orgs/:orgId/api-keys`,
   shown once, stored hashed, scoped to `session.create`), and calls the
   endpoint with `Authorization: Bearer seyd_live_…` and `for: <its user
   id>`, so "who was driving" survives into the audit log and Seyd stores no
   human identity. The page receives only the short-lived token. The key
   alone authorizes: no per-robot grant is needed for the `for` subject, and
   the key may mint `drive` or `observe` for any robot of its organisation,
   so the user's backend decides who gets which scope.
2. **A signed-in console user** with a session-creating role and a grant on
   the robot (the console's "Open pilot" button). Right for an operator team
   that uses the Seyd console as its fleet UI.
3. **Nobody**, for a public-grant robot. Demos only.

The pilot puts the token in `<seyd-video token>` or
`SeydSessionOptions.token`; it is presented to the signal server and again
in the first message to the robot, which verifies the signature and the
`aud` (the robot id). A robot with no public grant offers a session only to
a pilot with a valid token.

## The audit log

`GET /api/v1/orgs/:orgId/audit` (admins and owners; the console's Audit
page): organisation and robot changes, enrolment tokens, grants,
invitations, member roles, API keys, and every session token minted with its
subject, scope and lifetime. With the console's presence view it answers
"who had control when".

## Console access is invite-only

The hosted console grants no organisation to anyone uninvited and
self-registration is off at the provider; an invitation link both creates
the account and joins the org at the invited role (`node dist/bootstrap.js
invite <org-id> <email> <role>`, or the console's Members page; no mail
transport yet, so the link is handed over). For a new customer on the
hosted cloud the first step is therefore: ask the Seyd operator for an
organisation and an owner invitation.

## Hosted or self-hosted

The hosted cloud: signal server and relay at
`wss://seyd-signal-flj7s44j4a-ew.a.run.app/ws` (Cloud Run, `europe-west1`),
console at `/console/`, docs at `/docs/`, Postgres in the EU. The address to
give people is `https://seydio.web.app`, a redirect to the same service.

Self-hosted: the cloud's source is not in the public repository; it is
offered to customers under a commercial self-hosting license, which comes
with the same container, a `docker compose` file (Postgres, Logto for OIDC,
the API and the prober) and the operator's guide. Bring your own OIDC
provider or run headless with API keys only (`docs/self-hosting-auth.md`).
Robot and pilot must share the `signal_url`. A self-hosted signal server in
development can set `SEYD_DEV_OPEN_ENROLMENT=1` and
`SEYD_DEV_ALLOW_ANONYMOUS=1` to remove enrolment and tokens for a local loop
(never in production). An integration plan for a customer without that
license uses the hosted cloud.

## The HTTP API (signal server origin, `/api/v1`)

| Method and path | Purpose |
|---|---|
| `GET /healthz`, `GET /api/v1/health` | Liveness |
| `GET /api/v1/robots` | Presence; without an account only public-grant robots |
| `GET /api/v1/me` | The signed-in user and memberships |
| `POST /api/v1/orgs` | Create an organisation |
| `GET /api/v1/orgs/:orgId/robots` | The org's robots with live presence |
| `GET`, `PATCH`, `DELETE /api/v1/robots/:robotId` | One robot |
| `POST /api/v1/robots/:robotId/grants`, `DELETE …/grants/:id` | Grants |
| `POST`, `GET /api/v1/orgs/:orgId/enrolment-tokens`, `DELETE …/:id` | Enrolment tokens |
| `POST /api/v1/enrol` | Redeemed by the robot with its public key |
| `GET /api/v1/orgs/:orgId/members`, `PATCH`, `DELETE …/members/:userId` | Members and roles |
| `POST`, `GET /api/v1/orgs/:orgId/invitations`, `DELETE …/:id`; `POST /api/v1/invitations/accept` | Invitations |
| `POST`, `GET /api/v1/orgs/:orgId/api-keys`, `DELETE …/:id` | API keys |
| `POST /api/v1/session-tokens` | Mint a session token |
| `GET /.well-known/seyd-session-jwks.json` | The key robots verify session tokens with |
| `GET /api/v1/orgs/:orgId/audit` | The audit log |

Signalling is on `/ws`, the relay on `/relay` (`docs/protocol/signal-v2.md`).
