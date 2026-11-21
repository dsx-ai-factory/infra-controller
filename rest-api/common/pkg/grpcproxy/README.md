# Managed proxy credential protection

Credential-bearing Core and Flow proxy calls use the site's Vault Transit
service before their workflow inputs are recorded in Temporal. The readable
request retains redaction placeholders; `protectedSecrets` carries a versioned
envelope. The site activity restores the original request immediately before
calling Core or Flow, then encrypts the complete successful reply before
returning it to Temporal. Backend errors retain their gRPC classification with
a fixed message, excluding upstream details and causes that could echo secrets.

This protects the generic proxy's named secret fields, including firmware
download authentication and device overrides, BMC/UEFI credentials, credential
rotation, and ExpectedMachine/ExpectedSwitch/ExpectedPowerShelf patch and
replace credentials. Existing REST request/response schemas do not change.
Handlers must continue to name every secret-bearing top-level protojson field.
Other bespoke workflows, downstream persistence, REST audit logging, and
application authorization have their own protection boundaries.

## Vault and service configuration

Reuse the existing site Vault and its authentication/renewal facilities. Create
a dedicated, non-exportable Transit key named `rest-proxy` with type
`aes256-gcm96` and `derived=true`. Mount name and key name are overridable; both
must be single path segments containing only letters, digits, `_`, or `-`.
Do not reuse a TLS private key or a public site identifier as encryption key
material. The services neither create keys nor rotate them automatically.

Give each service its own externally renewed token, restricted to the dedicated
key. Both REST and the site agent need encrypt and decrypt because requests
and replies travel in opposite directions. Neither needs key administration:

```hcl
path "transit/encrypt/rest-proxy" { capabilities = ["update"] }
path "transit/decrypt/rest-proxy" { capabilities = ["update"] }
```

Configure the API and site-agent charts with matching values, and a separate
`credentialsSecret` for each service identity. For the API these values live
under the umbrella chart's `nico-rest-api` key; for the standalone site-agent
chart they are top-level values:

```yaml
proxyVault:
  address: https://vault.vault.svc.cluster.local:8200
  namespace: ""
  mount: transit
  key: rest-proxy
  credentialsSecret: rest-proxy-vault-credentials
```

The existing Secret must contain `token` and `ca.crt`; provision and maintain it
through the site's Secret management system. The charts only mount it, without
generating random keys, tokens or TLS material during rendering. The API runs
as UID/GID 1000; retain file readability for that identity. These are chart
defaults and can be overridden by values. The chart's explicit Vault
environment variables take precedence over the site-agent's `envConfig`;
the API rejects competing entries in `extraEnv`.

For deployments outside these charts, set the following environment variables
on both processes. All three required values must be nonempty:

| Variable | Contract |
| --- | --- |
| `GRPC_PROXY_VAULT_ADDR` | Required HTTPS origin; no user info, query, fragment or path other than `/`. |
| `GRPC_PROXY_VAULT_TOKEN_FILE` | Required path to the externally renewed token file; surrounding whitespace is removed. |
| `GRPC_PROXY_VAULT_CA_FILE` | Required path to PEM CA certificates used to verify Vault's TLS connection. |
| `GRPC_PROXY_VAULT_NAMESPACE` | Optional Vault namespace; empty uses the root namespace. |
| `GRPC_PROXY_VAULT_MOUNT` | Transit mount; defaults to `transit` when omitted or empty. |
| `GRPC_PROXY_VAULT_KEY` | Transit key name; defaults to `rest-proxy` when omitted or empty. |

Each invocation reloads CA configuration, and every Vault operation rereads the
token file. TLS verification cannot be disabled. Vault calls have a five-second
limit within the existing request/activity deadline, without application-level
retries. Missing configuration or failed encryption returns HTTP 503 before
Temporal submission; there is no plaintext or site-ID-key fallback. A failed
reply decryption returns HTTP 502. Non-secret proxy requests retain their
existing transport and do not require Vault.

## Persisted format, rotation and recovery

Envelope version 1 stores the Transit key name, Transit ciphertext (which
includes its key version), and a typed public context. Domain
`nico-grpc-proxy/v1`, site ID, service, method, workflow ID, canonical readable
request SHA-256 and direction form both key-derivation context and AEAD
associated data. Vault manages
nonces and authenticated encryption. The receiver recomputes the context from
its configured site, registered backend and actual workflow execution ID;
the API checks the reply against its submitted request. Foreign versions, key
names, contexts and mixed legacy/managed inputs are rejected. This binds an
envelope to a logical workflow ID, not a Temporal run ID, and does not provide
an independent anti-replay or authorization mechanism.

Rotate versions of the same Transit key using the site's Vault operations.
Retain old versions and keep `min_decryption_version` low enough for retained
histories and recoverable backups. Changing the configured key name does not
migrate histories and causes envelope key-name validation to fail. Reinstall
must restore the same Vault key history; replacing it with a newly created key
of the same name cannot decrypt previous ciphertext. Back up Vault state and
Temporal consistently with the site's recovery requirements; tokens and CA
trust must also be restored or reprovisioned.

Deploy the site-agent readers and their Vault access before the REST senders.
Protected calls use distinct `InvokeCoreGRPCProtected` /
`InvokeFlowGRPCProtected` workflows and activities; predecessor workers cannot
execute them by silently ignoring the envelope. Replace all old pollers before
admitting protected traffic to avoid unsupported-type task failures. Rolling
back REST leaves protected histories dependent on compatible site agents and
the same Vault key versions; do not remove those readers while executions or
restorable histories still need them.

Legacy workflows without credential envelopes retain their existing behavior.
To drain predecessor workflows carrying `encryptedSecrets`, temporarily set
`GRPC_PROXY_ALLOW_LEGACY_SECRETS=true` on the site agent (any other value rejects
them before backend dispatch). The site-agent chart exposes this as
`proxyVault.allowLegacySecrets`, default `false`. The legacy reader remains
site-ID-derived and cannot repair confidentiality of old histories or results.
New REST senders never write that format. Disable the legacy reader after the
drain and review historical payloads, exports and backups under the retention
policy; rotate still-valid credentials where exposure warrants it.

The proxy does not retry non-idempotent mutations. If reply encryption fails
after Core/Flow accepted a call, the caller sees failure even though the backend
may have changed state. Inspect backend state before retrying; encryption does
not make dispatch and history persistence atomic.

## Real Vault verification

`protection_vault_test.go` uses the `vault_integration` build tag. It requires
a fresh, disposable Vault dev server on a loopback HTTPS listener with dev root
token `fixture-token`. It creates the Transit mount/key and a restricted token;
discard the test server afterward. Set `GRPC_PROXY_VAULT_ADDR` and
`GRPC_PROXY_VAULT_CA_FILE` to that server and its CA, and leave
`GRPC_PROXY_VAULT_NAMESPACE` empty. From `rest-api/`, run:

```bash
GOMAXPROCS=4 GOFLAGS='-p=2 -tags=vault_integration -run=TestVaultProtector_TransitRotation$' make test-common
```

This checks real Transit encryption, context authentication, version rotation,
decryption-version floors and fresh-client recovery. It does not test a Vault
snapshot restore or a production rollout.
