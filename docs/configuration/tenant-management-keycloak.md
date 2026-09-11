# Tenant Management with Keycloak

Keycloak-side administration for onboarding a Tenant: creating the realm roles that
NICo reads as org membership, creating the Tenant's identity, and granting the
privileged Tenant capability.

[Tenant Management](tenant_management.md) covers the NICo side of the Day 1 workflow
with `nicocli`, and states that meeting its role and membership conditions is an
identity-provider task. This page is that task, for deployments running Keycloak.

NICo never writes to Keycloak. It reads role claims out of each request's token and
derives org membership from them, so every step below is performed against Keycloak
directly and none of it has a NICo API equivalent.

## Before You Start

You should already have:

- A NICo deployment with `keycloak.enabled: true`, which is the default for
  `helm-prereqs/setup.sh` installs.
- Keycloak realm administrator credentials. The bundled dev instance uses
  `admin` / `admin`.
- The `PROVIDER_ADMIN` role in your own org, for the NICo-side steps.

Confirm the deployment is in Keycloak mode and read the values you will need:

```bash
kubectl -n nico-rest get configmap nico-rest-api-config \
  -o jsonpath='{.data.config\.yaml}' | grep -A 8 '^keycloak:'
```

A `setup.sh` install reports:

```yaml
keycloak:
  baseURL: http://keycloak.nico-rest:8082
  clientID: nico-rest
  clientSecretPath: /var/secrets/keycloak/client-secret
  enabled: true
  externalBaseURL: http://keycloak.nico-rest:8082
  realm: nico
  serviceAccount: true
```

<Warning title="Plain HTTP is for the bundled deployment only">
`setup.sh` deploys Keycloak for development and evaluation, reachable over plain HTTP on a
cluster-internal Service. Every example on this page sends a credential, either a client
secret or a password, to that `http://` endpoint, which is acceptable only because the
traffic stays inside the cluster. A production deployment has to terminate TLS and set
`baseURL` and `externalBaseURL` to the `https://` address, since the issuer NICo requires
is built from `externalBaseURL`.
</Warning>

Three of these govern the rest of this page.

`realm` is the only realm NICo reads. A single `nico-rest-api` release accepts tokens
from exactly one realm, so the Provider org and every Tenant org are realm roles inside
`nico`. There is no list-valued realm setting.

`externalBaseURL` fixes the issuer NICo requires: a token is accepted only when its `iss`
claim equals `<externalBaseURL>/realms/<realm>`, which is
`http://keycloak.nico-rest:8082/realms/nico` above. It does not constrain where you fetch
the token from, only what Keycloak must have stamped into it.

That distinction matters because the bundled Keycloak sets no `KC_HOSTNAME`, so it derives
`iss` from the hostname each request arrives on. Requesting a token at
`http://localhost:8082` therefore stamps `iss` as `http://localhost:8082/realms/nico`,
which does not match, and the API rejects it with `401`. Requesting it at the in-cluster
name is the simplest way to get a matching `iss`, which is why the examples below run from
inside the cluster.

`serviceAccount: true` permits `client_credentials` tokens. Leave it enabled if the
Tenant will authenticate as a service account rather than as a user.

If `keycloak.enabled` is `false`, your deployment uses the `issuers` block instead and
this page does not apply. Onboarding a Tenant is then a configuration change rather than
realm administration, and the recipe is the "Provider with Multiple Tenant IdPs" example in
[Authentication and Authorization](/rest-api-reference/authentication-and-authorization).

That page is also the authoritative field reference for the `keycloak` block itself, so use
it when you need the meaning of a setting rather than the procedure for using it.

## How NICo Derives Orgs and Roles

NICo reads the `realm_access.roles` claim and splits each realm role on a colon. The
part before the colon is the org name and the part after it is the role:

```text
acme-corp:TENANT_ADMIN     ->  org "acme-corp", role TENANT_ADMIN
acme-infra:PROVIDER_ADMIN  ->  org "acme-infra", role PROVIDER_ADMIN
```

Five properties of this mapping matter when you create roles.

- **Exactly one colon.** A role with none or with two is discarded without an error,
  which is why realm roles such as `admin` and `user` have no effect in NICo.
- **Org names are lowercased.** Use a lowercase org name, and use the same value in the
  `{org}` path segment of every request and in `api.org` in `~/.nico/config.yaml`.
- **Both role spellings are accepted.** `TENANT_ADMIN` and the legacy prefixed forms
  `NICO_TENANT_ADMIN` and `FORGE_TENANT_ADMIN` all match. The bundled realm uses the
  prefixed form. Use the unprefixed form for new roles.
- **Groups work indirectly.** Roles inherited from a Keycloak group are present in
  `realm_access.roles`, so assigning the realm role to a group and adding users to that
  group is equivalent to assigning it to each user.
- **Role changes are cached for up to one minute.** NICo stores the derived org data on
  the user record and refreshes it when it is older than that, so a role added in
  Keycloak takes effect on a request made after the cache expires.

The three roles and what each grants are listed in
[Organization & Permissions](org-permissions.md). A Tenant needs `TENANT_ADMIN` in its
own org. A Provider needs `PROVIDER_ADMIN` in the Provider org, because inviting a
Tenant, creating Allocations, and granting capabilities are all Provider-side
operations.

Human users additionally need an `oidc_id` user attribute, which is the key NICo uses to
locate or create the user record. The bundled `nico-rest` client already publishes it
through an `oidc-usermodel-attribute-mapper`, so no mapper work is required, but the
attribute has to be set on each user. Service accounts do not need it, because NICo uses
the token's `sub` claim when a `client_id` claim is present.

## Opening an Admin Session

The Keycloak Admin CLI ships inside the container image. Confirm it is present:

```bash
kubectl -n nico-rest exec deployment/keycloak -- \
  bash -c 'test -x /opt/keycloak/bin/kcadm.sh && echo FOUND || echo MISSING'
```

Open a shell and authenticate against the container's local listener on port `8080`:

```bash
kubectl -n nico-rest exec -it deployment/keycloak -- bash

/opt/keycloak/bin/kcadm.sh config credentials \
  --server http://localhost:8080 --realm master \
  --user admin
```

Omitting `--password` makes `kcadm` prompt for it, which keeps the value out of the
process arguments and the shell history of a pod other operators can also exec into. The
bundled instance uses `admin`. To script the login instead, pass the password on stdin
rather than as an argument:

```bash
/opt/keycloak/bin/kcadm.sh config credentials \
  --server http://localhost:8080 --realm master --user admin <<'EOF'
admin
EOF
```

The admin console is an alternative for operators who prefer a UI. The bundled instance
publishes no ingress, so reach it with
`kubectl -n nico-rest port-forward svc/keycloak 8082:8082` and open
`http://localhost:8082`. Using the console for administration is fine; the in-cluster
restriction described above applies only to token requests.

## Onboarding a Tenant Org

The examples use org `acme-corp` for the Tenant and realm `nico`.

### Create the Tenant's realm role

```bash
/opt/keycloak/bin/kcadm.sh create roles -r nico \
  -s name=acme-corp:TENANT_ADMIN \
  -s 'description=NICo Tenant Administrator for acme-corp'
```

### Create the Tenant's identity

Choose one of the two options below.

**Option A, a service-account client.** Suited to automation, and the pattern the
bundled `ncx-service` client uses. Requires `serviceAccount: true` in the NICo config.

Pass the client definition on stdin rather than putting the secret in `-s secret=...`,
which would place it in shell history, process listings, and any terminal capture. Build
the JSON with an encoder rather than interpolating the secret into a string, so a secret
containing a quote, backslash, or newline cannot corrupt the payload:

```bash
CLIENT_SECRET_FILE=/path/to/client-secret

python3 -c '
import json, sys
with open(sys.argv[1]) as f:
    secret = f.read().rstrip("\n")
json.dump({
    "clientId": "acme-corp-service",
    "enabled": True,
    "publicClient": False,
    "serviceAccountsEnabled": True,
    "standardFlowEnabled": False,
    "directAccessGrantsEnabled": False,
    "secret": secret,
}, sys.stdout)
' "$CLIENT_SECRET_FILE" \
  | /opt/keycloak/bin/kcadm.sh create clients -r nico -f -

/opt/keycloak/bin/kcadm.sh add-roles -r nico \
  --uusername service-account-acme-corp-service \
  --rolename acme-corp:TENANT_ADMIN
```

**Option B, a human user.** The bundled realm ships no human users, so create one, set
`oidc_id`, and assign the role.

Keycloak 24 enables the declarative user profile with `unmanagedAttributePolicy` set to
`DISABLED`, so `oidc_id` is not a recognized attribute and a request setting it is
accepted while the value is discarded. Allow it in the admin context once per realm
before creating any user:

```bash
/opt/keycloak/bin/kcadm.sh update users/profile -r nico \
  -s unmanagedAttributePolicy=ADMIN_EDIT
```

`ADMIN_EDIT` keeps the attribute writable through the admin API while leaving it out of
end-user profile forms, which is the policy Keycloak recommends over `ENABLED`.

Keycloak 24's default user profile requires `email`, `firstName`, and `lastName`, and the
password grant fails with `Account is not fully set up` when any one of them is unset.
Read the realm's own list rather than trusting this one, since a customized profile can
require more:

```bash
/opt/keycloak/bin/kcadm.sh get users/profile -r nico
```

That check runs during authentication rather than being recorded on the account, so a
user missing one still reports an empty `requiredActions` and looks complete in the Admin
UI. Note that `emailVerified` is a separate field, so setting it to `true` does not imply
`email` itself has a value. Set all three at creation:

```bash
/opt/keycloak/bin/kcadm.sh create users -r nico \
  -s username=tenant-admin@acme-corp.example \
  -s email=tenant-admin@acme-corp.example \
  -s firstName=Tenant \
  -s lastName=Admin \
  -s emailVerified=true \
  -s enabled=true \
  -s 'attributes.oidc_id=["acme-corp-admin-001"]'

/opt/keycloak/bin/kcadm.sh add-roles -r nico \
  --uusername tenant-admin@acme-corp.example \
  --rolename acme-corp:TENANT_ADMIN
```

Set the password as a separate step, leaving `--new-password` off so `kcadm` prompts for
it. Passing it as an argument would expose the Tenant's password in the shell history and
process list of a pod other operators can exec into:

```bash
/opt/keycloak/bin/kcadm.sh set-password -r nico \
  --username tenant-admin@acme-corp.example
```

`kcadm` also reads the value from stdin, so a heredoc works when onboarding is scripted.

`set-password` sets a permanent password. Adding `--temporary` would leave an
`UPDATE_PASSWORD` required action and fail the same way.

Confirm the account is usable before handing it over. `requiredActions` has to be empty,
with `oidc_id` present, which also proves the policy change above took effect:

```bash
/opt/keycloak/bin/kcadm.sh get users -r nico \
  -q username=tenant-admin@acme-corp.example \
  --fields id,username,email,firstName,lastName,enabled,requiredActions,attributes
```

`kcadm` omits fields that are unset, so read this by what is missing: `email`,
`firstName`, `lastName`, and `attributes` all have to appear. An empty `requiredActions`
does not on its own mean the account can authenticate.

Any value for `oidc_id` works as long as it is unique within the realm and stable for
the life of the user. Changing it later makes NICo treat the login as a new user.

The doubled `u` in `--uusername` is not a typo. `kcadm.sh add-roles` prefixes each
option with the target type, so `--uusername` names a user and `--cclientid` names a
client, while `set-password` in the previous command takes a plain `--username`.

Exit the pod shell when finished.

### How a human user signs in

There is no browser step. `nicocli` implements the OAuth password grant, the client
credentials grant, and refresh-token renewal, and has no authorization-code or device-code
flow, so Keycloak's login page is never shown. `nicocli login` collects the username and
password (prompting when they are not supplied) and exchanges them at the realm's token
endpoint directly. This requires `directAccessGrantsEnabled` on the client, which the
bundled `nico-rest` client has.

Two `nicocli` flag defaults do not match a `setup.sh` deployment and have to be overridden:
`--keycloak-realm` defaults to `nico-dev` and `--client-id` defaults to `nico-api`, both of
which are Kustomize dev values.

`--keycloak-url`, `--keycloak-realm`, and `--client-id` are global flags, so they go before
`login`. Only `--client-secret`, `--username`, and `--password` belong to the subcommand.
Putting a global flag after `login` fails with `flag provided but not defined`.

Put the client secret in the config file rather than in `--client-secret`, which would
record it in shell history and expose it in the process list. `nicocli` reads
`auth.oidc.client_secret` when the flag is absent, and `~/.nico/config.yaml` is the file
`nicocli` already keeps the resulting token in:

```bash
mkdir -p ~/.nico
install -m 600 /dev/null ~/.nico/config.yaml
cat > ~/.nico/config.yaml <<'EOF'
api:
  base: http://localhost:8388
  org: acme-corp
  name: nico
auth:
  oidc:
    client_secret: REPLACE_ME
EOF
```

`nicocli login` prompts for the password, so it never needs `--password` either:

```bash
nicocli \
  --keycloak-url http://keycloak.nico-rest:8082 \
  --keycloak-realm nico \
  --client-id nico-rest \
  login \
  --username tenant-admin@acme-corp.example
```

`--keycloak-url` decides where the token is minted, and the bundled Keycloak stamps `iss`
from the host the request arrives on, as described above. Any endpoint is fine as long as
the request still carries the `externalBaseURL` hostname, which is what makes the minted
`iss` match. With the in-cluster default that name does not resolve from a workstation, so
interactive sign-in from outside the cluster needs one of:

- Set `externalBaseURL` to an externally resolvable hostname and expose Keycloak through an
  ingress, restricted to the endpoints listed in
  [Authentication and Authorization](/rest-api-reference/authentication-and-authorization).
  This is the production answer.
- Or, for evaluation only, port-forward Keycloak and map the in-cluster name to `127.0.0.1`
  in `/etc/hosts`, then keep the in-cluster name in `--keycloak-url`. The `/etc/hosts` entry
  is what keeps the configured hostname on the request, so the issuer in the minted token
  still matches.

The Keycloak admin console is for realm administration, not for Tenant sign-in. Human
Tenants never need an account in it.

### Verify the token maps to the org

Only Option A needs a token request here. An Option B user already minted one with
`nicocli login` in the previous section, so there is nothing to repeat for them. The
checks from the NICo call onward apply to both.

For Option A, request a token from inside the cluster. The request body carries the
client secret, so pass it to `curl` on stdin with `-K -` rather than in `-d`. In `-d` it
would appear in the pod spec, in Kubernetes audit records, and in your shell history.
Form-encode the secret as well, because a raw `&`, `=`, `+`, or `%` would otherwise
change the value that reaches Keycloak.

```bash
CLIENT_SECRET_FILE=/path/to/client-secret

TENANT_TOKEN=$(
  python3 -c '
import sys, urllib.parse
with open(sys.argv[1]) as f:
    secret = f.read().rstrip("\n")
body = urllib.parse.urlencode({
    "grant_type": "client_credentials",
    "client_id": "acme-corp-service",
    "client_secret": secret,
})
print("data = \"%s\"" % body)
' "$CLIENT_SECRET_FILE" \
  | kubectl run -i --rm --restart=Never --image=curlimages/curl "curl-tenant-$$" \
      -n nico-rest --quiet -- \
      -sf -K - "http://keycloak.nico-rest:8082/realms/nico/protocol/openid-connect/token" \
  | python3 -c 'import sys,json; print(json.load(sys.stdin)["access_token"])'
)
```

The secret reaches `curl` on stdin, so it never becomes a process argument. `curl` sets
`Content-Type: application/x-www-form-urlencoded` for `data` itself, and `data` implies
`POST`.

`helm-prereqs/keycloak/get-token.sh` does the same thing for the bundled `ncx-service`
client and is a working reference for the pattern.

Then confirm NICo resolves the identity, using a port-forward for the API call:

```bash
kubectl -n nico-rest port-forward svc/nico-rest-api 8388:8388 &

curl -sS "http://localhost:8388/v2/org/acme-corp/nico/user/current" \
  -H "Authorization: Bearer $TENANT_TOKEN"
```

For an Option B user, `nicocli user get` calls that same endpoint with the token from
`nicocli login`, so it answers the same question without a port-forward. Use the `curl`
form when you want the raw response. `nicocli login` stores the access token at
`auth.oidc.token` in `~/.nico/config.yaml`, which is where to read it from for
`$TENANT_TOKEN`.

A `200` response means the issuer validated, a realm role parsed into org
`acme-corp`, and the user record exists. Decode the token payload if it does not, then
check that `realm_access.roles` contains `acme-corp:TENANT_ADMIN`:

JWT segments use the URL-safe base64 alphabet, which GNU `base64 --decode` rejects, so
decode the payload with Python rather than piping it through `base64`:

```bash
printf '%s' "$TENANT_TOKEN" | cut -d. -f2 | python3 -c '
import base64, json, sys
segment = sys.stdin.read().strip()
padded = segment + "=" * (-len(segment) % 4)
print(json.dumps(json.loads(base64.urlsafe_b64decode(padded)), indent=2))
'
```

## Completing the Setup in NICo

With the realm role in place, the remaining steps are the standard flow in
[Tenant Management](tenant_management.md). Two points are specific to this path.

**The Tenant must call `nicocli tenant current` before accepting an invitation.** That
call creates the Tenant record and links any invitation matching the org name. Until it
runs, reading the Tenant Account returns `403` and accepting it returns
`404 Org does not have tenant`, because the account's `tenantId` is still empty.

**A Tenant should not call `nicocli service-account current`.** In a deployment with
`serviceAccount: true`, that endpoint creates an Infrastructure Provider, a Tenant, and
an already-accepted Tenant Account for the calling org, which makes the org its own
Provider instead of a Tenant of yours. It is the bootstrap path for the Provider org, not
for an invited Tenant.

The order is:

1. Provider Admin creates the invitation. See
   [Creating the Link](tenant_management.md#creating-the-link).
2. Tenant Admin runs `nicocli tenant current` to create the Tenant and link the
   invitation.
3. Tenant Admin accepts, moving the account to `Ready`. See
   [Accepting the Invitation](tenant_management.md#accepting-the-invitation-tenant-side).
4. Provider Admin creates one Allocation per Site, which is what gives the Tenant
   capacity. Refer to [Assigning Resources with Allocations](tenant_management.md#assigning-resources-with-allocations).

## Granting the Privileged Tenant Capability

Nothing about `targetedInstanceCreation` is Keycloak-specific. It is not a realm role and
it has no claim: a Provider Admin grants it on the Tenant Account after the Tenant
accepts, using `siteCapabilities`.

```bash
nicocli tenant-account update \
  --data '{"siteCapabilities":[{"siteIds":[],"targetedInstanceCreation":true}]}' \
  <account-id>
```

See [Granting Targeted Instance Creation](tenant_management.md#granting-targeted-instance-creation)
for the payload rules, the per-Site override behavior, how the effective value resolves,
plus how to read the current value.

The distinction worth keeping straight on this page is that Keycloak decides **who the
caller is and which org they act in**, while the Tenant Account decides **what that org is
allowed to do**. A realm role of `acme-corp:TENANT_ADMIN` makes someone a Tenant Admin for
`acme-corp`; it does not make them privileged. Adding a role such as
`acme-corp:PROVIDER_ADMIN` does not grant the capability either. It authorizes Provider
operations for `acme-corp`, and it does not create an Infrastructure Provider. That
bootstrap is `nicocli service-account current`, described above.

## Making Realm Changes Permanent

Changes made with `kcadm.sh` or the admin console live in Keycloak's database. They
survive pod restarts, but they are not represented in your configuration, so a rebuilt
realm loses them.

The bundled Keycloak imports `helm-prereqs/keycloak/realm-configmap.yaml` with
`--import-realm`, which imports only a realm that does not already exist. **Editing that
file and re-running `setup.sh` does not change an existing realm.** To fold a Tenant org
into the reproducible baseline, add its roles and identities to the file and re-import
from a clean state:

```bash
helm-prereqs/keycloak/clean.sh
helm-prereqs/keycloak/setup.sh
```

`clean.sh` drops the `keycloak` database and also deletes the `keycloak-client-secret`
Secret, which the `nico-rest-common` sub-chart owns, so the API cannot mount its secret
until the chart is reapplied. Re-run the upgrade with the same values and image
coordinates the install used, then restart the API so it re-reads the signing keys from
the rebuilt realm:

```bash
helm upgrade --install nico-rest helm/rest/nico-rest \
  --namespace nico-rest \
  -f helm-prereqs/values/nico-rest.yaml \
  --set global.image.repository="$NICO_IMAGE_REGISTRY" \
  --set global.image.tag="$NICO_REST_IMAGE_TAG"

kubectl -n nico-rest rollout status deployment/nico-rest-api --timeout=120s
kubectl -n nico-rest rollout restart deployment/nico-rest-api
```

Restarting the API while Keycloak is unavailable leaves it running with Keycloak
authentication inactive, so confirm Keycloak is ready first.

## Troubleshooting

| Symptom | Cause | Resolution |
|---------|-------|-----------|
| `401 Invalid authorization token in request` | Token `iss` is not `<externalBaseURL>/realms/<realm>`. Usually a token fetched over a port-forward, where Keycloak stamped the forwarded hostname instead | Decode the payload and compare `iss` with the configured issuer, then fetch the token from a host that produces a matching value |
| `401 Service accounts are not enabled` | Token carries a `client_id` claim but `keycloak.serviceAccount` is `false` | Enable `serviceAccount` in the values and upgrade, or use a user token |
| `nicocli login` fails with `authentication failed: Account is not fully set up` | Usually `email`, `firstName`, or `lastName` is unset, all three of which Keycloak 24's default user profile requires. Any one of them is enough to cause it. The validation runs at authentication time, so `requiredActions` is empty and the Admin UI shows nothing wrong. A temporary password produces the same message, through an `UPDATE_PASSWORD` action that *is* recorded | Read the account with `kcadm.sh get users --fields email,firstName,lastName` and treat any absent field as the cause, since `kcadm` omits unset fields. `emailVerified: true` does not mean `email` is set. Fill them in with `kcadm.sh update users/<id>`, checking `get users/profile` for a customized required set. Clear `requiredActions` only if it is non-empty |
| `401 Failed to retrieve or create user record, DB error` on a human user's first request, with a valid token | The `oidc_id` claim is empty because the attribute was discarded, and NICo rejects an empty user key rather than creating a row. Keycloak 24 defaults `unmanagedAttributePolicy` to `DISABLED` | Set the policy to `ADMIN_EDIT`, re-set `oidc_id` on the user, then decode the token and confirm the claim is present |
| `403 Requested organization not found in token claims` | The `{org}` path segment does not match any role prefix. Often a case mismatch | Use the lowercase org name in the path and in `api.org` |
| `403 User does not have any roles assigned` | No realm role parsed into an org. Usually a role name without exactly one colon | Check `realm_access.roles` in the decoded token |
| `403 User does not have Tenant Admin role with org` | Role parsed, but it is not `TENANT_ADMIN` | Assign `acme-corp:TENANT_ADMIN` and retry after the one-minute cache expires |
| `404 Org does not have tenant` when accepting | `nicocli tenant current` has not been run for the Tenant org | Run it, then accept |
| `400 Tenant Account status is not Invited` | The account is already `Ready` | No action needed, the invitation was already accepted |
| A role added in Keycloak has no effect | Org data cached on the user record | Retry after one minute |
| Realm edits to `realm-configmap.yaml` do not appear | `--import-realm` skips an existing realm | Apply with `kcadm.sh`, or re-import from clean |

## Related Documentation

- [Tenant Management](tenant_management.md), the NICo-side Day 1 workflow with `nicocli`
- [Authentication and Authorization](/rest-api-reference/authentication-and-authorization),
  the Day 0 auth configuration reference for both the `keycloak` and `issuers` modes
- [Organization & Permissions](org-permissions.md), the role model and what each role grants
- [Quick Start Guide](../getting-started/quick-start.md), deployment and token acquisition
  for the bundled realm
- [Reference Installation](../getting-started/installation-options/reference-install.md),
  deployment-side authentication wiring including the `issuers` alternative
