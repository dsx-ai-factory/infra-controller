# Keycloak for NICo REST

Self-contained Keycloak deployment for `nico-rest-api` authentication.
Uses `quay.io/keycloak/keycloak:24.0` with `--import-realm` to seed the
`nico` realm on first boot.

## Quick Start

```bash
./keycloak/setup.sh    # deploy
./keycloak/clean.sh    # tear down
```

## Realm: `nico`

| Setting | Value |
|---------|-------|
| Keycloak URL | `http://keycloak.nico-rest:8082` |
| Token endpoint | `http://keycloak.nico-rest:8082/realms/nico/protocol/openid-connect/token` |

### Clients

| Client ID | Type | Secret | Realm roles (via service account) |
|-----------|------|--------|-----------------------------------|
| `nico-rest` | API client (audience only; no end-user login) | `nico-local-secret` | — |
| `ncx-service` | Service account (M2M, client_credentials) | `nico-local-secret` | `ncx:NICO_PROVIDER_ADMIN`, `ncx:NICO_TENANT_ADMIN` |

### Users

There are **no human users** in this realm. The only identity is the auto-created
`service-account-ncx-service` pseudo-user that backs the `ncx-service` client;
its sole purpose in the realm JSON is to map the `ncx:NICO_*` realm roles onto
tokens issued via `client_credentials`.

## Acquiring a Token (ncx-service)

Tokens must be obtained through the cluster-internal Keycloak URL so the
JWT issuer matches what `nico-rest-api` expects. Use the helper script:

```bash
./get-token.sh                        # prints the token and nothing else
TOKEN=$(./get-token.sh)               # capture for later curl calls
```

Use `./example.sh` if you also want the JWT payload decoded and the API
exercised against `/v2/org/ncx/nico/user/current`.

Or the raw curl equivalent:

```bash
TOKEN=$(kubectl run -i --rm --restart=Never --image=curlimages/curl curl-token \
  -n nico-rest --quiet -- \
  -sf -X POST http://keycloak.nico-rest:8082/realms/nico/protocol/openid-connect/token \
  -H "Content-Type: application/x-www-form-urlencoded" \
  -d "grant_type=client_credentials&client_id=ncx-service&client_secret=nico-local-secret" \
  | python3 -c "import sys,json; print(json.load(sys.stdin)['access_token'])")
```

### Decode the JWT payload

```bash
echo $TOKEN | cut -d. -f2 | awk '{l=length($0)%4; if(l) $0=$0 substr("====",1,4-l)}1' | base64 --decode | python3 -m json.tool
```

Example payload:

```json
{
    "iss": "http://keycloak.nico-rest:8082/realms/nico",
    "aud": "nico-rest",
    "azp": "ncx-service",
    "realm_access": {
        "roles": ["ncx:NICO_PROVIDER_ADMIN", "ncx:NICO_TENANT_ADMIN"]
    },
    "preferred_username": "service-account-ncx-service",
    "client_id": "ncx-service"
}
```

## Calling the API

Run it all end-to-end (fetch token + hit the user endpoint + dump the DB row):

```bash
./example.sh
```

Manually:

```bash
TOKEN=$(./get-token.sh)

# Port-forward to nico-rest-api
kubectl port-forward -n nico-rest svc/nico-rest-api 18388:8388 &

# Health check
curl -s http://localhost:18388/healthz | python3 -m json.tool

# Current user — auto-creates the service-account row in nico.user on first call
curl -s http://localhost:18388/v2/org/ncx/nico/user/current \
  -H "Authorization: Bearer $TOKEN" | python3 -m json.tool
```

Example response:

```json
{
    "id": "46ec5c59-60c7-4429-b528-5a2d575b38f6",
    "firstName": "ncx-service",
    "lastName": "",
    "email": "",
    "created": "2026-04-16T08:27:36.033706Z",
    "updated": "2026-04-16T08:39:30.510956Z"
}
```

## How It Works

The API parses `realm_access.roles` from the JWT and splits each role on `:`
to extract the org name. For example, `ncx:NICO_PROVIDER_ADMIN` means
org=`ncx`, role=`NICO_PROVIDER_ADMIN`. The org must match the `{orgName}`
path parameter in the URL (`/v2/org/ncx/...`).

On first API call, `nico-rest-api` auto-creates a row in the `"user"`
table keyed by the token's `sub` (the service-account user's UUID) with
`org_data` populated from the realm roles.

## Files

| File | Purpose |
|------|---------|
| `realm-configmap.yaml` | Realm JSON (roles, clients, service-account role map) |
| `deployment.yaml` | Keycloak Deployment |
| `service.yaml` | ClusterIP Service (8082 -> 8080) |
| `setup.sh` | Creates secrets, DB, applies manifests |
| `clean.sh` | Deletes resources and drops DB |
| `get-token.sh` | Fetches an ncx-service token (client_credentials) |
| `example.sh` | End-to-end demo: token + API + DB |

## After `helm-prereqs/setup.sh` finishes

When `keycloak.enabled: true` in `values/nico-rest.yaml`, the main
`helm-prereqs/setup.sh` prints a short "How to get a token for ncx-service"
block at the end of the run. That block shows the exact in-cluster curl
required, the client_id / client_secret / token endpoint, and the
`curl -H "Authorization: Bearer $TOKEN"` call against
`/v2/org/ncx/nico/user/current` so you can verify the realm roles flow
through to the API.
