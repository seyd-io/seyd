# Identity in a self-hosted Seyd

Seyd separates *who you are* from *what you may do*. The identity provider
answers only the first question; orgs, roles, robot grants and the audit trail
are Seyd's and live in Seyd's Postgres (ADR 0007).

That split gives self-hosting customers (the cloud is offered under a
commercial self-hosting license; see *Run the cloud yourself*) three shapes
to choose from.

## 1. Bundled — use the identity provider we ship

`docker compose up` starts Logto beside the API. Nothing to
configure; create an account in the Logto admin console on first run.

Choose this if you have no identity provider of your own, or don't want Seyd
touching the one you have.

## 2. Bring your own identity provider

Point Seyd at any OIDC provider — Keycloak, Zitadel, Entra ID, Okta, Google
Workspace, your own. Seyd verifies tokens against its JWKS and never sees a
password.

```bash
SEYD_OIDC_ISSUER=https://login.yourcompany.com/realms/main
SEYD_OIDC_AUDIENCE=https://seyd.yourcompany.com     # the API's resource indicator
# The console's client id. Only needed if your access tokens omit `email` —
# most providers' resource-scoped tokens do, and their userinfo endpoint will
# not accept such a token, so the profile arrives as a verified ID token.
SEYD_OIDC_CLIENT_ID=seyd-console

# Optional: map your groups onto Seyd roles.
SEYD_OIDC_ROLE_CLAIM=groups
SEYD_OIDC_ROLE_MAP='{"robotics-admins":"admin","robotics-pilots":"operator"}'

# Who gets in, and at what role.
SEYD_PROVISIONING=default-org       # or invite-only
SEYD_DEFAULT_ORG_ID=<uuid>
SEYD_DEFAULT_ROLE=viewer            # when no group claim matches
```

Two more variables exist for the case where the browser and the API reach the
provider at different addresses — a container network, or an internal hostname
behind a public one. The issuer must match the `iss` claim in the token, which
is the browser-facing URL, so the API needs the internal endpoints given
explicitly rather than discovered:

```bash
SEYD_OIDC_JWKS_URI=http://idp.internal:3001/oidc/jwks
SEYD_OIDC_USERINFO_URI=http://idp.internal:3001/oidc/me
```

### Choosing a provisioning policy

| `SEYD_PROVISIONING` | Who gets an account | Who decides the role |
|---|---|---|
| `self-serve` | Anyone who can sign in; they get their own org | They own it |
| `invite-only` | Only people an admin invited, matched on a **verified** email | The invitation |
| `default-org` | Everyone your IdP authenticates, into one org | `SEYD_OIDC_ROLE_MAP`, else `SEYD_DEFAULT_ROLE` |

`default-org` is the usual choice for a single-tenant deployment: your IdP is
already the gate, so Seyd should not be a second one.

Under `default-org`, group claims are authoritative and re-applied on every
login — demote someone in your directory and they are demoted in Seyd. Under
the other two policies Seyd's roles win, so an IdP claim can never silently
demote an owner.

Under `invite-only` you will usually also close self-registration at the
provider, or anyone can still create an account there and sit on the "no
organisation yet" page. Seyd then has to open the door for invitees itself: an
invitation asks the provider for a one-time sign-in token and hands out one
link that both registers the account and joins the org. For Logto this is
`SEYD_LOGTO_M2M_CLIENT_ID/SECRET` (`cloud/README.md`, "Invitations under a
closed door"); for another provider it is an implementation of `UserInviter`
in `cloud/api/src/authn/inviter.ts`, the only place that speaks a provider's
management API. Leave registration open and configure nothing, and invitations
work the older way: the invitee signs up first, then the link accepts.

An invitation is always honoured, whatever the policy, because it is an
explicit act by an administrator. It matches only on a **verified** email:
without that check, anyone could claim an invitation by typing the address into
their own IdP.

## 3. Headless — Seyd inside your product

If your application already has users, Seyd does not need to know about them.
Your backend authenticates them, holds a Seyd API key, and mints short-lived
pilot session tokens:

```http
POST /api/v1/session-tokens
Authorization: Bearer seyd_live_1a2b3c4d_…
Content-Type: application/json

{ "robot_id": "rover-12", "scope": "drive", "ttl_sec": 300, "for": "your-user-id" }
```

The response is an ES256 JWT the browser hands to `SeydSession`. `for` is
recorded in Seyd's audit log, so "who was in control at 14:32" survives into
your system too.

No identity provider is involved in this shape at all: leave
`SEYD_OIDC_ISSUER` unset and no one can sign in to the console, which is
usually what you want when Seyd is a component rather than a product.

## What is never delegated

Two things stay Seyd's whichever shape you pick, and both are deliberate:

- **Robot identity.** A robot generates its own Ed25519 key and enrols with a
  token; it has no user account and never authenticates against your IdP. A
  fleet keeps working while your identity provider is down.
- **Pilot session tokens.** Minted with Seyd's key, verified by the agent
  against `/.well-known/seyd-session-jwks.json`. The agent never talks to an
  identity provider.

## Migrating between providers

Users are keyed on `(oidc_issuer, oidc_subject)`. To move, match on verified
email and rewrite those two columns:

```sql
update users set oidc_issuer = 'https://new-idp/oidc', oidc_subject = $new
 where lower(email) = lower($email);
```

Memberships, grants, robots and the audit log reference `users.id`, which does
not change. Nothing else moves.
