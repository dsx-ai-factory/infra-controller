# Node-Auth: Self-Signed Bearer JWTs for Scout and DPU-Agent

Design for [#355](https://github.com/dsx-ai-factory/infra-controller/issues/355)
(sub-issue of the Vault-elimination epic
[#195](https://github.com/dsx-ai-factory/infra-controller/issues/195)): Scout and the
DPU-agent authenticate to the API with short-lived bearer JWTs alongside —
and eventually instead of — mTLS client certificates.

Nodes sign their own tokens with the private key of their **existing** mTLS
client certificate. There is no new key material anywhere, no server-side
signing key or key storage, and no dependency on the `machine_identity`
(tenant JWT-SVID) subsystem. **No node-auth JWT is issued or refreshed by the
API**: nodes re-mint locally, so this design adds no token-issuance or refresh
RPC to the API. (The API does still issue the *certificate* those tokens are
signed with, through the existing discovery/attestation flow — that is the
credential this design reuses, not one it replaces.) The agent serves a local
`GetNodeToken` RPC to processes on its own DPU, which distributes tokens it
has already minted rather than acting as an issuance authority.

On a DPU the dpu-agent is the only holder of that key *for the purpose of
authenticating to nico-api*: co-located NICo pods get finished tokens from it
over a local unix socket instead of mounting the key
(see [Key distribution on DPF](#key-distribution-on-dpf-the-agent-is-the-token-broker)).
The key has not left the node entirely — otelcol still mounts the credentials
directory, because it needs the certificate for TLS client auth to its OTLP
gateway, which a nico-api bearer token cannot replace.
Approaches that were weighed and rejected are recorded under
[Designs not used](#designs-not-used).

It is distinct from the tenant-facing [SPIFFE JWT-SVID design](spiffe-svid-sdd.md):
that issues identity tokens *to tenant workloads* via IMDS; this design covers
how *NICo's own node agents* authenticate to the NICo API.

## How it works

```text
node (scout / dpu-agent)                      nico-api
------------------------                      --------
has /opt/forge/machine_cert.pem  ── x5c ──►   1. verify x5c chain against the
and /opt/forge/machine_cert.key                  client-cert root CA (same roots
                                                 as the TLS listener)
mint ES256 JWT signed with the                2. verify JWT signature with the
cert's own key, 5-min TTL,                       verified leaf's public key
cert chain in the x5c header                  3. enforce exp / iat / aud, bounded
                                                 lifetime
attach as Authorization: Bearer               4. SPIFFE-validate the leaf; map its
on every gRPC request                            URI SAN through the same
                                                 SpiffeContext as mTLS certs
                                              ⇒ identical machine principal, RBAC
                                                 unchanged
```

Key-less co-located services (fmds) never touch the cert or key at all: they
ask the dpu-agent for its current token over a unix socket and attach that
same header.

```text
fmds pod                          dpu-agent                nico-api
--------                          ---------                --------
no key, no cert    ──socket──►   holds the key,   ── same bearer token ──►
GetNodeToken                      mints as above           (identical
(cached, refreshed                                          verification)
 in the background)
```

## Auth flow: new DPU → first authorized gRPC call

**Provisioning & bootstrap (no credentials yet)**

- A new DPU is provisioned (BFB installed via DPF); the `forge-dpu-agent` DPF
  service starts on it. Two config inputs are sufficient to get through
  bootstrap: the API endpoint and the root CA bundle (`forge_system.root_ca`).
  The cert/key files at `/opt/forge/machine_cert.pem` / `machine_cert.key`
  don't exist yet. Node-auth has no per-node configuration: every node mints
  the fixed `nico-api` audience once its certificate lands.
- The agent opens a TLS connection to `nico-api` that is **server-auth only**
  — it verifies the API's cert against the root CA but presents no client
  credential (the API listener uses `allow_unauthenticated()`, so the
  connection is accepted with an anonymous principal).

**First credential: the machine certificate**

- The agent calls `DiscoverMachine`
  (`host-support/registration.rs::register_machine`) carrying its hardware
  enumeration (`DiscoveryInfo`); this RPC is reachable pre-credential by
  design — it is how a machine with no credential obtains its first one, so it
  cannot itself require one.
- **What authorizes it is the network, not a credential.** The handler
  (`handlers/machine_discovery.rs`) resolves the caller from the connection's
  source IP: `find_optional_for_update_by_ip` maps the peer address to a
  registered `machine_interface`, and the machine that interface belongs to is
  the one that gets a certificate. A caller cannot name the machine it wants
  to be. An already-allocated host running scout may pass a
  `machine_interface_id`, but it is checked against the source IP
  (`find_for_update_if_matches_instance_ip`); a mismatch is refused as
  `PermissionDenied` and logged as a potential impersonation attempt.
- The strength of that boundary is therefore the strength of the provisioning
  network: anything that can source packets from a registered machine's IP,
  before that machine has enrolled, can obtain its certificate — and hence
  mint node-auth tokens as it. Node-auth inherits this; it neither weakens nor
  strengthens it, because the JWT is signed by the very key this exchange
  delivers. Sites that need more should enable attestation, where hosts take
  the `AttestQuote` path and issuance is gated on a TPM challenge instead.
- `allow_insecure_discovery` bypasses the IP check entirely and trusts a
  caller-supplied `machine_interface_id`. It is for integration tests, logs a
  warning when it fires, and must stay off in production.
- The API registers/matches the machine and returns a `machine_certificate`
  in the response: a Vault-PKI-issued EC P-256 leaf whose SAN is the machine's
  SPIFFE URI (`spiffe://<trust-domain>/<ns>/machine/<machine-id>`), plus the
  issuing CA and private key. (On attestation-enabled sites, hosts get this
  via `AttestQuote` after a TPM challenge instead; DPUs take the discovery
  path.)
- `write_certs` persists leaf+issuing-CA to `/opt/forge/machine_cert.pem` and
  the key to `/opt/forge/machine_cert.key`. **This existing cert key is the
  JWT signing key — nothing else is ever created.**

**Minting the JWT (client side, `rpc::node_jwt`)**

- The agent's gRPC client was built with
  `ForgeClientConfig::new(root_ca, ClientCert{...}).with_node_jwt()`, so a
  `NodeJwtMinter` watches those two file paths.
- On the next outgoing RPC, the minter reads cert+key from disk (re-encoding
  Vault's SEC1 key PEM to PKCS#8) and signs an **ES256 JWT**: header
  `{alg: ES256, x5c: [leaf, issuing CA]}`, claims
  `{sub: <SPIFFE URI from its own cert SAN>, aud: "nico-api",
  iat: now, exp: now+300s}`. The token is cached and re-minted when < 60 s
  remain — no refresh RPC, and cert renewal is picked up automatically
  because the files are re-read at each mint.
- Renewal writes the certificate and the key in two separate operations, so
  the minter compares the key's public half against the one certified by the
  leaf before signing. A mismatch (a mint landing between the two writes)
  fails the mint rather than caching a token the API will reject.
- `BearerAuthService` stamps `Authorization: Bearer <jwt>` onto the request.
  (The TLS channel may still present the client cert too — dual-support; each
  is sufficient alone.)

**Validation (server side, requires `[node_auth] enabled = true` + TLS listener)**

- The authn middleware sees the Bearer header and hands it to
  `NodeJwtValidator`, which checks in order:
  - header `alg` is exactly ES256 (no algorithm substitution);
  - the `x5c` chain verifies against the **same root CA file the TLS listener
    uses for client certs** (`[tls] root_cafile_path`) — path building,
    validity window, client-auth EKU;
  - the JWT signature verifies with the *verified leaf's* public key;
  - claims: `exp` and `aud` are validated by `jsonwebtoken`; `iat` is
    required when `NodeClaims` deserializes and is independently checked to be
    no later than `exp` or the API's clock-skew allowance. The validator also
    bounds both `exp - iat` and how far `exp` may reach into the future by
    `max_token_ttl_sec` (default 900 s), so a client can't stretch `exp`;
  - the leaf passes SPIFFE validation (leaf-only, single URI SAN), and `sub`
    must equal that SAN — identity comes from the verified cert, never from
    the claim.

**Authorization & the first call**

- The validated SPIFFE URI is mapped through the same `SpiffeContext` as mTLS
  certs → a `SpiffeMachineIdentifier` principal, byte-identical to what the
  cert path would have minted.
- Casbin RBAC evaluates that principal exactly as before (machine class →
  Agent/Scout role rules) — **no RBAC changes** — and the handler executes.
  That is the first authorized gRPC call on bearer auth.

Note: the very first *authorized* call after discovery could ride either
credential, since the agent holds both from the same moment — the JWT only
becomes load-bearing once `mtls_enabled = false`.

## Q1 — How does the agent get a private key that is trusted to create the JWT?

**It already has one.** The node's Vault-issued mTLS client certificate key
(`/opt/forge/machine_cert.key`, EC P-256 — Vault PKI role `key_type=ec,
key_bits=256`) signs the JWT, and the certificate itself rides along in the
token's `x5c` header (RFC 7515 §4.1.6). The key is trusted because the
certificate chains to the root CA the API already trusts for client certs —
the JWT is effectively "mTLS at the application layer".

Bootstrapping is unchanged: a machine obtains its first certificate through
the existing discovery/attestation flow (`DiscoverMachine` / `AttestQuote`
respond with the machine certificate — see the auth-flow walkthrough above),
and from that moment it can mint tokens. Minting is best-effort: before the
cert exists, requests simply carry no bearer header.

## Q2 — JWT side by side with mTLS

The authn middleware (`CertDescriptionMiddleware` in `crates/authn`) mints
principals from **both** sources on every request: the TLS-layer client cert,
and the `Authorization: Bearer` token (validated by `NodeJwtValidator`). Both
paths converge on the same SPIFFE URI → `SpiffeMachineIdentifier` mapping
through the same `SpiffeContext`, so RBAC (Casbin policy, role mapping) is
completely unchanged. A node presenting both credentials gets the same
principal twice — harmless. Scout configures a bearer-token provider with
`ForgeClientConfig::with_node_jwt()` and DPU-agent with
`ForgeClientConfig::with_token_provider()`, regardless of whether node-auth is
enabled. The provider attaches the `Authorization` header only when it can
obtain a token; before the certificate exists, requests carry no bearer header.
A server with node-auth disabled ignores any header that is present.

**Enabling** is therefore order-independent: node and API images can be rolled
in either order, because a token nobody validates is inert and a node that
cannot mint yet simply sends no header.

**Disabling is not.** Once fmds is deployed in token mode, turning
`[node_auth] enabled` off stops the API accepting bearer tokens immediately
while fmds returns to cert mode only as DPF rolls the DaemonSet — see
[Known issues](#known-issues) for the resulting window and
[Disabling node-auth](#disabling-node-auth) for the order that avoids it. The
symmetry claim applies to the enable direction only.

## Q3 — JWT off by default, configured in the API config

```toml
[node_auth]
enabled = false          # master switch for accepting bearer JWTs
max_token_ttl_sec = 900  # upper bound on client-chosen lifetimes (cap 86400)
# fmds_use_node_tokens   # optional; unset follows `enabled`. Only for staging
                         # a change — see "Disabling node-auth" below.
```

All `[node_auth]` values are read at API startup; changing any of them requires
an API restart. The defaults and validation contract is:

| Setting | Default and accepted values | Startup behavior |
| --- | --- | --- |
| `enabled` | `false` or `true`. | `false` installs no bearer validator. `true` requires a TLS listener and readable `[tls] root_cafile_path`; the API refuses startup rather than accepting bearer tokens over plaintext. |
| `max_token_ttl_sec` | `900` seconds; with bearer auth enabled, an integer from `300` (the node's fixed minted lifetime) through `86400`, inclusive. | `0`, `1`–`299`, and values greater than `86400` fail startup when `enabled = true`. When bearer auth is disabled, this inactive value is not otherwise validated. |
| `mtls_enabled` | `true` or `false`; it controls only machine mTLS identities. | Setting both `enabled` and `mtls_enabled` to `false` always fails startup because no node credential would remain. Service and admin client certificates are unaffected. |
| `fmds_use_node_tokens` | Unset; then follows `enabled`. An explicit boolean is a rollout override. | `true` with `enabled = false` always fails startup, because that deployment would make fmds present tokens to an API that rejects them. |

The `aud` claim is always `"nico-api"`, defined once in
`rpc::node_jwt::NODE_JWT_AUDIENCE` and used by every minter and by the API
validator. It has no configuration, CLI flag, or Helm value. `[node_auth]`
rejects unknown keys, including a legacy `audience` setting, rather than
silently accepting a value that cannot change token validation.

The entire preflight runs *before* DPF resource creation, so a
misconfiguration cannot mutate cluster state on its way to failing.

## Q4 — mTLS on by default, disableable in the API config

```toml
[node_auth]
mtls_enabled = true
```

When `mtls_enabled = false`, the middleware stops minting machine principals
from client certificates — bearer JWTs become the only node auth path. The
gate is scoped to **machine** certs: service and admin-CLI certs on the same
listener are unaffected. `enabled = false` + `mtls_enabled = false` is
rejected at startup (node lockout).

Note the trust chain is still the certificate PKI: disabling mTLS here
disables the *transport-layer* cert authentication, not cert issuance. Nodes
must keep renewing certificates because the JWT is signed by the cert key.

Both mechanisms depend on `listen_mode = "tls"`, though only one says so.
Bearer tokens are refused over plaintext at startup; machine mTLS just quietly
has nothing to work with, because the plaintext accept path hands the
middleware an empty peer-certificate list. So the both-off lockout check
guarantees a working path only on a TLS listener -- on plaintext,
`mtls_enabled = true` passes it while authenticating nobody. Nothing shipped
selects a plaintext mode, which is why this is documented rather than
enforced: requiring TLS whenever `mtls_enabled` is set would break local
plaintext development to prevent a misconfiguration that only matters if you
expected node auth to work in the first place.

## Q5 — Key regeneration and public-key exchange

**Regeneration** is the existing client-certificate renewal: when
`ClientCertRenewer` rotates the cert/key files, the minter picks the new pair
up on its next re-mint (it re-reads both files from disk each time). No
coordination, no state.

**Public-key exchange: the x5c header.** Every token carries the certificate
chain that vouches for its signing key, and the API verifies that chain
against the root CA bundle it already holds (`[tls] root_cafile_path`). There
is no JWKS endpoint, no key registry, and no key distribution problem — CA
rotation is handled wherever the root bundle is handled today.

An `x5c` chain contains at most four certificates, including the leaf. The
node refuses to mint a token when its client-certificate bundle exceeds that
limit, emits a `node_auth` warning at most once every five minutes per process,
and continues with mTLS only. The API rejects an over-limit token before Base64-
decoding its certificates or building a verification path. Keep the node's
bundle within the limit; otherwise, a deployment with `mtls_enabled = false`
has no node authentication path.

**CA rotation** moves both consumers of that bundle together. The TLS
listener already re-reads the file every five minutes for cert-manager
rotations; the validator's trust anchors are held behind a lock and refreshed
on that same tick, so a token chaining to a freshly rotated CA is accepted
without an API restart. A failed reload keeps the previous anchors — a bundle
caught mid-write must not disarm node auth.

**Compromise response** is likewise the PKI's: a stolen key/cert pair is the
same incident as a stolen mTLS cert today. Two cases, with very different
bounds — do not conflate them:

- **A captured token** is bounded by expiry. It is replayable only until its
  own `exp` (see the replay window in Design decisions), against the same
  `aud`, over TLS. Waiting is a sufficient response.
- **A compromised private key** is not bounded by token expiry at all. The
  holder mints fresh, valid tokens whenever it likes, for as long as the
  *certificate* remains acceptable. The validator calls
  `allow_unknown_revocation_status()`, so revoking the certificate does not
  stop it either: the only things that do are the certificate's own expiry,
  or removing the issuing CA from the bundle the API trusts — which cuts off
  every machine under that CA, not just the compromised one.

So key compromise requires incident response, not patience — and it is worth
being precise about what each response actually achieves:

- **Re-issuing the machine's credential is recovery, not containment.** It
  gets the legitimate node onto a fresh key. It does nothing to the attacker,
  who keeps minting acceptable tokens from the old certificate until that
  certificate expires, because the validator does not check revocation.
- **Rotating the issuing CA is the containment step**, and the only one
  available today. It invalidates the stolen certificate immediately — along
  with every other certificate under that CA, so every machine has to
  re-enrol. That is the cost, and it is why this is a decision rather than a
  runbook step.
- **Waiting for expiry** contains it eventually, bounded by the certificate's
  lifetime rather than the token's.

Short token lifetimes limit what a *captured token* is worth; they do nothing
to limit a stolen key. Cutting a compromised key off surgically — without
taking the whole CA with it — needs revocation checking we do not do yet.

## Q6 — JWT best-practice checklist

| Practice | How it's honored |
| --- | --- |
| Asymmetric signing, no `alg` confusion | ES256 only; both the header check and `Validation` pin the algorithm, so `none`/HS256 substitution is rejected. |
| Identity never comes from claims | The principal derives from the **chain-verified certificate's SPIFFE SAN**; `sub` is only cross-checked against it. A forged `sub` buys nothing. |
| Short-lived tokens | Clients mint 300 s tokens; the server enforces `exp - iat` and `exp - now` ≤ `max_token_ttl_sec` (default 900 s, hard cap 86400), so a client cannot stretch `exp`. |
| `exp` / `iat` / `aud` enforced | `jsonwebtoken` validates `exp` and `aud`; `NodeClaims` requires `iat`, and the validator rejects an `iat` after `exp` or more than its clock-skew allowance in the future. It also bounds `exp - iat` and `exp - now`. |
| Chain validation, not pinning | `x5c` verified with rustls `WebPkiClientVerifier` (path building, validity window, client-auth EKU) against the same roots as the TLS listener. |
| SPIFFE leaf constraints | `carbide_authn::validate_x509_certificate` re-checks leaf-ness, key usage, and the single-URI-SAN rule — same code path as mTLS certs. |
| No bearer tokens over plaintext | Enforced at both ends. Server: startup refuses `enabled = true` on a non-TLS listener, the middleware only installs the validator when the listener is TLS-terminated, and a failed acceptor rebuild keeps the previous acceptor rather than falling back to cleartext. Client: `with_token_provider` implies `require_tls_enforcement`, and building a token client against a non-HTTPS URL is an error. |
| No key material at rest beyond the PKI | The server holds no signing key; the client holds only what it already had, and `write_certs` persists the key file owner-only (0600). Credentials never in logs (both token caches have a redacting `Debug`). |
| Least exposure for the signing key | The dpu-agent is the only process that signs node-auth tokens: consumers get finished short-lived ones over a local socket, and token-mode fmds never mounts the credentials directory. Not yet absolute — otelcol still mounts it, since it needs the certificate for TLS client auth to its OTLP gateway. |

## Component map

| Piece | Where |
| --- | --- |
| Client mint + cache + header injection | `crates/rpc/src/node_jwt.rs` (`NodeJwtMinter`, `BearerAuthService`, `NodeTokenProvider`) |
| Client opt-in | `ForgeClientConfig::with_node_jwt()` / `with_token_provider()` (`crates/rpc/src/forge_tls_client.rs`); called in scout `client.rs` and dpu-agent `lib.rs` |
| Key-less token source | `crates/rpc/src/node_token_socket.rs` (`SocketTokenSource`), consumed by fmds via `--node-token-socket` |
| Broker service | `AgentLocal/GetNodeToken` in `crates/agent/proto/agent_local.proto`, with bindings generated once by `carbide-rpc` and served by `crates/agent/src/local_api.rs` |
| Server validation | `crates/api-core/src/node_auth.rs` (`NodeJwtValidator`) |
| Config | `NodeAuthConfig` in `crates/api-core/src/cfg/file.rs` (`[node_auth]`) |
| Middleware hook | `BearerTokenAuthenticator` trait + machine-cert gate in `crates/authn/src/middleware.rs` |
| Wiring | `crates/api-core/src/setup.rs` (preflight, validator construction), `listener.rs` (middleware install, trust-anchor reload), `dpf_services.rs` (fmds token mode) |
| Charts | `bluefield/charts/nico-fmds` (`useNodeTokens`), `bluefield/charts/nico-dpu-agent` |

All of it logs under one target: `RUST_LOG=node_auth=debug` turns on the
whole feature's tracing, and every message carries a `node-auth:` prefix.

## Key distribution on DPF: the agent is the token broker

The machine cert/key live at `/opt/forge` on the DPU, a hostPath directory
several NICo pods mount. Rather than share the key with each of them, the
dpu-agent is the only holder and hands out finished tokens.

### `AgentLocal/GetNodeToken` contract

`GetNodeTokenRequest` has no fields: callers neither send a current token nor
an identity for the agent to inspect. Authorization is the local socket mount
and its mode, not a request field. On success, `GetNodeTokenResponse.token` is
an ES256 JWT for the agent's machine identity and `expires_at` is its UNIX-time
expiration in seconds. Callers must treat `expires_at` as the only freshness
signal; the same token string may be returned repeatedly.

The agent returns a cached token only when it had more than 60 seconds left at
the agent's cache check. Otherwise it reads the current certificate and key and
mints a replacement with the normal 300-second lifetime. This is not a
single-flight operation: concurrent cache misses can mint more than one valid
token, and callers must not infer ordering or uniqueness from the response.

| Cache and credential state | Result | State after the call |
| --- | --- | --- |
| Cached token has more than 60 seconds remaining | `OK` with that cached token and its original `expires_at`. | Cache is unchanged. |
| Cache is absent or has 60 seconds or less remaining, and minting succeeds | `OK` with a replacement token and its new `expires_at`. | The replacement becomes the cached token. |
| Cache is absent or too close to expiry, and the certificate/key cannot be read, parsed, or matched | `UNAVAILABLE`: `no node token available yet; machine certificate not present or unreadable`. | No usable token is returned; a stale cache entry, if any, is not served. |

The method defines no application deadline and performs no server-side retry.
Raw gRPC callers choose their own deadline and retry policy. The bundled
`SocketTokenSource` bounds each connect-plus-RPC attempt to five seconds,
retries failures every five seconds in one background loop, refreshes at 60
seconds remaining, and stops serving its own cache at 30 seconds remaining;
its request path never waits for the RPC. Apart from transport failures, the
only status returned by this handler is `UNAVAILABLE` for a token that cannot
be minted. Concurrent requests are safe but may independently mint on a cache
miss as described above.

- **The socket.** The agent serves `AgentLocal/GetNodeToken` on a unix socket
  at `/opt/forge/run/agent.sock`, mode 0600 — a dedicated `run/` subdirectory
  so a consumer can be given the socket without the credentials beside it. The
  mount is read-only: connecting does not write to the filesystem, and Linux
  exempts socket inodes from the read-only-mount check, so `connect(2)`
  succeeds. Access is gated by the socket's 0600 mode. `bind` creates
  the socket at umask permissions and the 0600 chmod only lands afterward, so
  the directory — not the socket — is what closes that window; the agent
  therefore requires the socket's parent to be dedicated, creating it `0700` or
  refusing a directory that holds anything else. (Without that rule,
  `local-api-socket = /run/agent.sock` would take `/run` to `0700` and lock
  every non-root service on the box out of its runtime files.) The path is
  configurable via `[forge-system] local-api-socket`; the server retries
  forever rather than dying once, since a bare-metal boot can start the agent
  before its directory exists. This socket is the consolidation point for
  future agent ↔ co-located-service traffic, in place of new sockets, ports,
  or file drops.
- **The consumer.** `SocketTokenSource` implements the same
  `NodeTokenProvider` trait as the minter, so a client built with
  `with_token_provider()` is indistinguishable on the wire from one that
  minted its own. A background task keeps the cached token fresh (refresh at
  60 s remaining, request-path cutoff at half that, per-attempt deadline
  covering connect + RPC); the request path never blocks, and before the
  first successful fetch requests simply carry no bearer header.
- **The trust anchor, without the key.** fmds still needs the root CA, so the
  agent mirrors it to `<certsDir>/pub/`, a directory holding nothing else.
  Token-mode fmds pods mount `pub/` plus `run/` and **do not reference the
  credentials volume at all**, not even in the init container — the machine
  key is absent from the pod rather than merely read-only, which matters
  because the container runs as UID 0. It is a directory mount, not a
  `subPath`, so the atomic-rename CA replacement still propagates into a
  running pod. The agent publishes from the init container, again at every
  start, and then reconciles the copy against the configured CA every five
  minutes — the same cadence the API listener re-reads it on. That last part
  is what makes rotation work: `pub/` is the *only* trust anchor a token-mode
  consumer has, since it does not mount the credentials directory, so a
  publish-once-at-startup mirror would pin fmds to the old issuer and break it
  the moment that issuer was retired. The reconcile compares before writing,
  so a steady state costs a read rather than churning the inode consumers are
  watching.
- **The switch.** fmds helm values are rendered by the API, so `useNodeTokens`
  *defaults* to the one setting that makes tokens meaningful: `[node_auth]
  enabled`. With node-auth off, the chart renders exactly as before.
  `fmds_use_node_tokens` overrides that default, which is what makes an
  ordered transition possible — see [Disabling node-auth](#disabling-node-auth).
  Rollout note: enabling it requires an agent image that serves the socket, so
  agent and fmds images must be deployed together.

Scope: this covers *authentication to nico-api*. otelcol also uses the machine
cert for TLS client auth to its OTLP gateway, which a nico-api bearer token
cannot replace, so the key only fully disappears from other pods once that
ingest path has its own credential story.

## Design decisions (resolved questions)

1. **How does the server learn the public key?** → from the token itself
   (`x5c`), verified against the existing root CA — the one key-distribution
   mechanism the system already operates.
2. **Why not drop mTLS immediately, since the JWT proves the same key?** →
   dual-support de-risks rollout and keeps requirement 4 orthogonal; and the
   JWT still depends on the cert PKI, so cert issuance must outlive transport
   mTLS.
3. **Client-side enable knob?** → none. Nodes always mint when they hold a
   cert; a disabled server ignores the header (verified by middleware test).
   One switch (`[node_auth] enabled`) controls the feature.
4. **Replay window** → bounded by what the *server* accepts, not by what our
   clients mint. A token captured from a stock scout or dpu-agent is replayable
   for ≤ 5 minutes (`NODE_JWT_TTL_SECS`), but a holder of the signing key can
   mint up to `max_token_ttl_sec` — 900 s by default, and the config permits up
   to 86400 — so that setting, not the client constant, is the number to reason
   about. Replay is against the same `aud` over TLS only. Accepted; `jti`/nonce
   tracking or DPoP-style proof-of-possession is the hardening path if needed,
   and lowering `max_token_ttl_sec` tightens it today.
5. **RSA machine certs** → not supported (Vault PKI role is EC P-256
   everywhere); the validator rejects non-EC leaves with a clear debug reason.

## Disabling node-auth

Enabling is order-independent (see Q2). Disabling is not, so it needs a
sequence rather than a single config change. The hazard is that the API stops
*accepting* bearer tokens the moment it restarts, while fmds keeps *presenting*
them until DPF has rolled every DaemonSet.

The safe path is to take fmds out of token mode first, while the API still
accepts tokens, and only then stop accepting them:

1. **Move fmds back to cert mode ahead of the API.** Leave `[node_auth]
   enabled = true` and set:

   ```toml
   [node_auth]
   enabled = true
   fmds_use_node_tokens = false
   ```

   Restart the API. It keeps accepting bearer tokens, but now renders fmds
   with `useNodeTokens: false`, so DPF rolls the DaemonSet back onto the
   machine client certificate with nothing depending on tokens in the
   meantime. The reverse combination — `fmds_use_node_tokens = true` with
   `enabled = false` — is refused at startup, since it deploys fmds to present
   tokens the API does not accept.
2. **Confirm the roll landed on every node** before touching the API. A single
   node still in token mode is a single node that loses `phone_home` in the
   next step, so check the thing that actually distinguishes the two modes —
   which volumes the pods carry — not just that they restarted:

   ```bash
   #!/usr/bin/env bash
   set -euo pipefail
   NS=dpf-operator-system
   SEL=app.kubernetes.io/name=nico-fmds

   # Exactly one DaemonSet, or stop: picking the first of several would verify
   # the wrong one, and zero means the selector or namespace is wrong.
   mapfile -t DS < <(kubectl get daemonset -n "$NS" -l "$SEL" -o name)
   [ "${#DS[@]}" -eq 1 ] || { echo "expected 1 DaemonSet, found ${#DS[@]}" >&2; exit 1; }

   # The roll must be finished before its result means anything. This is a
   # precondition, not a nicety: the pod list below is a single sample, so
   # checking it mid-roll can show every pod already converted while others
   # have not been recreated yet.
   kubectl rollout status "${DS[0]}" -n "$NS" --timeout=10m

   want=$(kubectl get "${DS[0]}" -n "$NS" -o jsonpath='{.status.desiredNumberScheduled}')
   ready=$(kubectl get "${DS[0]}" -n "$NS" -o jsonpath='{.status.numberReady}')
   [ -n "$want" ] && [ "$want" -gt 0 ] && [ "$ready" = "$want" ] \
     || { echo "daemonset not fully ready: ready=$ready desired=$want" >&2; exit 1; }

   mounts=$(mktemp)
   trap 'rm -f "$mounts"' EXIT

   # What each pod actually mounts -- not what it declares. A volume can be
   # declared and never mounted, which would read as a completed roll.
   kubectl get pods -n "$NS" -l "$SEL" \
     -o jsonpath='{range .items[*]}{.spec.nodeName}{"\t"}{.metadata.name}{"\t"}{range .spec.containers[*]}{range .volumeMounts[*]}{.name}{" "}{end}{end}{"| init: "}{range .spec.initContainers[*]}{range .volumeMounts[*]}{.name}{" "}{end}{end}{"\n"}{end}' \
     | tee "$mounts"

   got=$(wc -l < "$mounts")
   echo "desired=$want ready=$ready observed=$got"
   ```

   **Do not proceed unless all of these hold:**

   - `kubectl rollout status` returned success and `ready` equals `desired`;
   - `$got` equals `$want`. Zero rows is a *failure*, not a pass -- a wrong
     namespace, a wrong label, or pods not yet recreated all produce an empty
     list that otherwise reads exactly like "nothing is in token mode any
     more";
   - every row mounts `nico-certs`;
   - no row mounts `nico-certs-pub` or `nico-agent-run`.

   The rollout check and the mount check answer different questions -- whether
   the roll finished, and which mode it finished *into* -- and you need both.
   A single node still in token mode is a single node that loses `phone_home`
   in the next step, so stopping here is much cheaper than discovering it
   after.
3. **Set `[node_auth] enabled = false` and restart the API.** Confirm
   `mtls_enabled = true` first — the pair being false is refused at startup,
   but confirming beforehand turns a failed rollout into a caught typo.
   `fmds_use_node_tokens = false` can stay: it now agrees with the derived
   value, and leaving it costs nothing.
4. **Verify** that node-auth is quiet: `RUST_LOG=node_auth=debug` should show
   no bearer validation, and `phone_home` should be succeeding from fmds.

To roll forward again, reverse it: turn `enabled` on and confirm the API
accepts tokens, then clear `fmds_use_node_tokens` (or set it to `true`) to
move fmds across.

Followed in this order there is no window where fmds presents a credential the
API refuses, so this is an ordinary rolling change rather than a maintenance
window. Skipping step 1 — flipping `enabled` off on its own — reopens the gap
described below.

## Known issues

**Disabling `[node_auth]` after fmds has been in token mode is unsequenced.**
The two halves of the switch take effect on different clocks:

- The API stops accepting bearer tokens *immediately*, when it restarts with
  `enabled = false`.
- fmds returns to cert mode only *eventually*. The re-apply is real —
  `create_initialization_objects` upserts the DPUServiceConfiguration by
  forced server-side apply, so the new `useNodeTokens: false` does land — but
  DPF then has to re-render and roll the DaemonSet across the fleet.

In between, fmds pods are still running token mode with no client cert and
tokens the API now rejects: `phone_home` and machine-identity signing take
401s until the roll reaches each node. Config serving and instance metadata
are unaffected, and it self-heals once the roll completes.

Nothing is corrupted by the transition. The agent writes `machine_cert.pem` /
`.key` and the base-path root CA unconditionally, in both modes, so cert mode
works the moment a pod rolls; leftover `pub/` and `run/` directories are
inert.

**This is avoidable, and [Disabling node-auth](#disabling-node-auth) is how.**
`[node_auth] fmds_use_node_tokens` overrides the derived value, so fmds can be
moved off tokens while the API still accepts them; sequenced that way there is
no window at all. What remains a known issue is that the *unsequenced* change
— flipping `enabled` off on its own — still produces the outage above, and
nothing stops an operator doing that. Setting the override the wrong way round
(`true` with `enabled = false`) is refused at startup, but the plain
single-switch disable is a legitimate-looking config change with a transient
cost.

The enable direction has the mirror window but fails safe on its own: the init
container blocks waiting for the CA in `pub/` rather than starting into a
broken state.

## Designs not used

**Server-issued tokens.** A site-level signing key in the credential store, a
`RefreshNodeToken` RPC, and DPU device identity as the refresh anchor. It
brings back everything this design deletes: a server-side key to store,
rotate, and share across HA replicas; issuance and refresh RPCs to build,
version, and rate-limit; and a bootstrap question of its own (what
credential authorizes the *first* refresh). Reusing the node's existing
client-cert key gets the same authenticated principal with no new secret
anywhere. Worth revisiting only if per-node keys are removed from the
architecture, at which point nothing is left to self-sign with.

**A JWKS endpoint or key registry.** The obvious way to publish per-node
public keys, and unnecessary: `x5c` carries the certificate with every token
and the API already holds the root CA that vouches for it. A registry would
add a distribution channel that can go stale, be unreachable, or disagree
with the PKI — and CA rotation would have to be handled in two places instead
of one.

**Reusing the tenant JWT-SVID subsystem.** `machine_identity` issues SPIFFE
JWT-SVIDs to *tenant workloads* via IMDS. Node agents authenticating to the
NICo API is a different problem with a different trust root, and coupling
them would make NICo's own control plane depend on a tenant-facing service
being healthy.

**Kubernetes Secret for the machine key.** Secret mounts are tmpfs, so the
key stops touching DPU flash, and sharing becomes explicit, per-pod, and
auditable through the K8s API. But in DPF the DPU-cluster control plane
(kamaji-hosted etcd) runs on the x86 management cluster, so the key gains a
durable copy — plus backups — *off the DPU*, requiring etcd encryption at
rest to be guaranteed; the agent needs new K8s API rights to create and
update the Secret; and scout (pre-DPF, live-image) can't use this path at
all, so the file mechanism would survive alongside it. The token broker
addresses the same concern — key exposure surface — without moving the key
anywhere, so this became moot rather than merely deferred.

**Sharing the key by hostPath, read-only.** What token mode replaced. A
read-only mount of the credentials directory stops nothing: the container
runs as UID 0 and can read `machine_cert.key` regardless. Whether the mount
is read-only is not the control; whether the file is in the pod's namespace
at all is.

**Mounting just the CA file by `subPath`.** A tempting one-line fix for the
above — mount the single file rather than the directory. `subPath`
bind-mounts an inode, and `install_bootstrap_ca` replaces the CA by atomic
rename, so a running pod would keep the old anchor forever and silently fail
after a CA rotation. Hence the dedicated `pub/` directory.

**A network endpoint for the broker.** There is precedent for it — the agent
already dials `nico-dhcp-server` over gRPC through a k8s `Service` — but the
token broker is a different kind of service, and a port is the wrong shape
for it:

- *The socket is the authorization check.* `GetNodeToken` ignores its request
  entirely: there is no authentication in the handler, because anyone who can
  connect is already on the node with the path mounted. On a TCP port that
  same RPC hands a full machine identity to every pod in the DPU cluster, and
  to anything that can route to the DPU.
- *TLS can't fix that.* Protecting the endpoint means mTLS with the machine
  cert — which the consumer does not have, that being the entire premise. The
  transport cannot be secured by the credential it exists to distribute, so a
  port needs a second credential system to bootstrap the first. The SPIFFE
  Workload API is a unix socket for this reason.
- *Locality.* Agent and consumer are DaemonSets on the same node; the call is
  same-node by construction. A `Service` routes it through cluster DNS and a
  VIP whose control plane (kamaji-hosted etcd) runs on the x86 management
  cluster — making "can this node authenticate at all" depend on remote
  infrastructure. A socket is a path.
- *No listening port* to firewall or expose by accident, which matters on a
  DPU where `nico-otelcol` runs with `hostNetwork: true`.

The cost of the choice is real, though smaller than it first appeared: the
socket sits in its own `run/` subdirectory so a consumer can mount it without
mounting the credentials next to it, but that mount can be read-only, because
socket inodes are exempt from the read-only-mount check. And it serves
co-located consumers only — anything off-DPU needing a node token is not
covered by this design.

Note what mode 0600 does and does not buy. It excludes other UIDs, which is
real — but the NICo pods that mount this directory (`dpu-agent`, `fmds`,
`otelcol`) all run as UID 0, so it separates none of *them* from each other.
Among those, the control that matters is which pods get the mount at all.
Against anything else on the DPU running as another user, the mode does its
usual job. The gain over sharing the key is the blast radius of a leak — a
short-lived token instead of a long-lived private key — not a hard boundary
between the NICo pods themselves.

**Certificate revocation checking.** Not implemented — see Q5. A compromised
key keeps minting valid tokens until the certificate expires, and revoking
the certificate does not stop that; if the incident model needs a faster
cutoff than certificate expiry or a CA rotation, revocation is the work, and
this design does not do it.
