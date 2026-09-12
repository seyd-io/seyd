# ADR 0007 — Authentication is pluggable; authorization is ours

**Status:** accepted (2026-09-04)

## Context

Seyd needed console login and sign-up, and PLAN.md §2.5 had deferred the
identity provider as open decision 1. The constraints were fixed by the
2026-08-28 product decisions: portable by construction, EU residency likely, no
Google lock-in, and a one-person team building with AI tools.

The question looked large and turned out to be small, because Seyd already has
**three identity planes** and only one of them was open:

| Plane | Mechanism | Status before this ADR |
|---|---|---|
| Robot identity | Ed25519 key generated on the robot, challenge-response over signal v2 | Built, provider-independent |
| Pilot session authorization | ES256 JWT minted with Seyd's own key, verified by the agent | Built, provider-independent |
| **Human identity** | console login | **Did not exist** |

Because the first two never touch an identity provider, a wrong choice here
costs a console migration, not a fleet outage: a robot in a field keeps
accepting pilots while the provider is down, mid-upgrade, or being replaced.
That reframing is what made the decision safe to take now rather than later.

The second observation is that **no identity provider can express what Seyd
needs to authorize**. "Anna may drive robot 42 but only observe robot 7, and
only while she holds the driver slot" is domain logic. Any product whose
authorization model we adopted would have to be worked around immediately.

We surveyed Keycloak, Zitadel, Ory, Authentik, Logto, Auth0/Okta, Clerk,
WorkOS, Stytch, Supabase Auth, Better Auth, Hanko, Casdoor, FusionAuth and the
roll-your-own libraries. Findings that decided it:

- **FusionAuth** paywalls `client_credentials` from ~$162/mo. Seyd issues
  machine credentials on day one; disqualifying.
- **Ory** ships OSS `.0` releases only, by stated policy, and every B2B feature
  is paid. Its Polis component (Apache-2.0, SAML + SCIM) remains a useful
  bolt-on.
- **Hanko** has no orgs, invitations, RBAC or M2M.
- **Lucia, Oslo, Arctic, OpenAuth and Auth.js** are all deprecated, abandoned
  or absorbed. Lucia's author concluded the OAuth 2.0 protocol "isn't an ideal
  layer to abstract into a library".
- **Better Auth** covers the whole requirement list in MIT packages with no new
  service, but carries a heavy advisory stream whose criticals cluster in
  exactly the B2B plugins we would adopt (sso, scim, api-key, organization),
  breaking changes on minor releases, and no OSS audit log. For a one-person
  team that is an ongoing tax rather than a one-time cost.
- **Supabase Auth** became an OIDC provider in late 2025 and is a credible
  conservative fallback, but has no orgs, roles, invitations or machine tokens.

## Decision

**Authentication is a pluggable edge. Authorization is the product and stays in
Seyd's Postgres.**

1. `cloud/api` verifies OIDC tokens against the issuer's JWKS. Issuer and
   audience are configuration; nothing else about the provider is load-bearing.
   Users are keyed on `(oidc_issuer, oidc_subject)`, so changing provider is an
   `UPDATE` of two columns matched on verified email, not a migration.
2. **Logto 1.43.0, self-hosted**, is the provider, running as one container
   beside our own Postgres in `cloud/docker-compose.yml`. It was chosen on
   footprint rather than features: since orgs, roles and grants are ours, the
   right provider is the smallest one that federates email+password, social
   login and — later — a customer's enterprise SSO behind a *single issuer*.
   A single issuer is the prize; it keeps multi-issuer account-linking out of
   the console forever.
3. Orgs, memberships, roles, invitations, robot grants, API keys and the audit
   log live in `cloud/api/db/migrations/001_init.sql`. The role matrix
   (`src/accounts/rbac.ts`) is a closed list, not a policy engine.
4. Pilot session tokens stay ES256, minted with Seyd's own key, published at
   `/.well-known/seyd-session-jwks.json`. They never depend on the provider.
5. API keys are Seyd's own `Authenticator`, always enabled regardless of which
   provider is configured — a customer's backend calling the API must not
   depend on our choice of identity provider.

Three provisioning policies cover the three deployment shapes, selected with
`SEYD_PROVISIONING`:

| Policy | For | Behaviour on first login |
|---|---|---|
| `self-serve` | Seyd's own SaaS | Creates an org the user owns |
| `invite-only` | A shared deployment | No access until an admin invites them |
| `default-org` | Single-tenant self-hosting | Joins one org at the role their group claim maps to |

A self-hosting customer plugging in their own identity provider configures
`SEYD_OIDC_ISSUER`, `SEYD_OIDC_AUDIENCE`, `SEYD_OIDC_ROLE_CLAIM` and
`SEYD_OIDC_ROLE_MAP`. An OEM embedding Seyd skips human identity altogether:
their backend authenticates their users, holds a Seyd API key, and mints pilot
session tokens through `POST /api/v1/session-tokens`.

## Consequences

- Login and sign-up exist, and the console is ours. The login *page* is the
  provider's, branded in its admin console — that is what a redirect flow
  means, and PLAN.md's earlier wording ("the login UI is ours") was corrected.
- We verify the **access token** issued for the API's resource indicator, not
  the ID token: an API's audience is the resource, the ID token's is the client.
- **A resource-scoped access token cannot carry the profile, and userinfo will
  not accept it.** Found by running it: Logto issues that token with `scope: ""`
  and nothing but `sub`, and `GET /oidc/me` answers 401 — correctly, since the
  token was not issued for the provider. So the API could authenticate a user
  and never learn their email, which silently breaks invitation matching, the
  members list and org naming. The console therefore also sends its **ID token**
  in `x-seyd-id-token` on `GET /api/v1/me`, verified against the same JWKS with
  `aud == SEYD_OIDC_CLIENT_ID`, **and its `sub` must equal the access token's** —
  otherwise a user could present a colleague's validly-signed ID token, write
  that email onto their own account, and claim the colleague's invitation. The
  userinfo fallback stays for providers whose access token is accepted there.
- The verifier deliberately does **not** pin a signing algorithm. Logto 1.43
  signs with ES384, Keycloak defaults to RS256, others use EdDSA; an allowlist
  would break login on every provider that chose differently. Safety comes from
  resolving the key out of the issuer's own JWKS. Seyd's own session tokens are
  pinned to ES256, because there we control both ends.
- In compose the browser and the API reach Logto at different addresses, so the
  `iss` claim (`http://localhost:3001/oidc`) is not resolvable from inside the
  API container. `SEYD_OIDC_JWKS_URI` and `SEYD_OIDC_USERINFO_URI` exist for
  exactly this, and are set to `http://logto:3001/…` there. Any deployment that
  puts the IdP behind an internal hostname hits the same thing.
- We take on a patch obligation. Logto shipped ten advisories in ten weeks
  through mid-2026 and CERT/CC publicly reported four unanswered emails to
  `security@logto.io` (VU#492466). The image is **pinned, never `latest`**, and
  checking its advisories is part of dependency review.
- Logto has **no SCIM** and no roadmap for it. When an enterprise customer
  requires directory provisioning, the answer is Ory Polis (Apache-2.0)
  alongside, not a provider migration.
- Presence is now scoped per org, and the unauthenticated `GET /api/v1/robots`
  returns only robots carrying a public grant. Both used to return everything,
  which was harmless when every robot was ours and a cross-tenant leak of robot
  ids and NAT reports the moment there is a second customer. Dev mode
  (`SEYD_DEV_ALLOW_ANONYMOUS=1`) keeps the old unscoped behaviour so the sim
  robot and the smoke harness still work with no account.
- `seydd enrol --token …` closes the loop the console opens; an enrolment token
  had nothing to redeem it before. It adds `reqwest` to `seydd` with
  `rustls-tls-webpki-roots` and no default features — no OpenSSL, so ARM
  cross-builds stay dependency-free.
- Postgres becomes a hard dependency of the deployed cloud. `MemoryAccounts` is
  a faithful implementation, not a stub, so tests and a laptop still run without
  it — but a divergence between the two is a bug in `PgAccounts`.
- Still open: whether Logto's corporate domicile is acceptable on a European
  customer's security questionnaire. If it is not, Zitadel (Swiss, AGPL) is the
  alternative and the seam makes it a configuration change. Also unresolved:
  Logto publishes no DPA, and there is no mail transport in the cloud, so
  invitation tokens are currently handed over by whoever issued them.

## Amendment 2026-09-10 — invite-only means closed registration, and an invitation opens the door

The console went live on the deployed cloud with `SEYD_PROVISIONING=invite-only`
and the owner asked that *only invited people be able to sign up*. Seyd's
policy alone does not deliver that: the provider's own registration is a
second door, and with it open anyone could create an account and sit on the
"no organisation yet" page. So the deployed Logto runs in sign-in-only mode,
and an invitation has to create the account itself.

Decided:

1. **An invitation mints the invitee's one-time sign-in token at the
   provider** (Logto's Management API, `POST /api/one-time-tokens`) and the
   invitation becomes one link: `console#/invite?email&token&ott`. The console
   starts the login with `one_time_token` + `login_hint`; Logto registers from
   a verified one-time token even with registration off, asks for a password,
   and reports `email_verified` (which it derives from the primary email
   existing). Seyd's invitation then matches on the verified email as before.
   This is the one place Seyd speaks a provider's management API; it sits
   behind `UserInviter` in `authn/` so another provider is another
   implementation, and a provider left with open registration needs none.
2. **The provider has no public admin surface.** Cloud Run exposes one port
   and Logto wants two; rather than a second service for the admin console,
   it is disabled deployed and run on a laptop against the same database when
   needed. `configure.mjs` sets up Seyd's objects through the Management API
   with the seeded proxy credential, so a deployment can be configured without
   ever creating an admin account by script.
3. **A formal email connector, no mail transport.** Logto refuses email as a
   sign-up identifier without an email connector, so its HTTP connector posts
   to the API, which logs the message. The invite flow needs no message; a
   password reset code therefore lands in the server log until a real
   transport exists — an accepted, documented stopgap, and the `MailSink`
   interface is where the first transport goes.
4. **The console gets the provider settings from the API** at boot
   (`GET /api/v1/console-config`), not from its build, so one build serves any
   deployment and changing provider stays an environment change.

Verified 2026-09-10: Logto 1.43.0 on Cloud Run (`seyd-logto`, Neon database,
seeded with `--encrypt-base-role` because Neon refuses a password-less role),
the console served at `/console/` from `seyd-signal`, one-time tokens minted
against the deployed provider, the console's Sign in reaching the provider's
email + password page with no "create account" link, and an invitation link
carrying the console through the provider straight to its set-a-password step
for an address that had no account. Setting that password and completing the
first login is a human step and was left to the owner.

