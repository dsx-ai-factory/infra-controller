# Dependency network qualification

This opt-in qualification exercises Core's PostgreSQL, Vault, and OTLP clients
against disposable PostgreSQL, Vault, and OpenTelemetry Collector processes.
CoreDNS serves controlled A and AAAA answers. It covers the remaining hostname
and address-family work in [#5403](https://github.com/dsx-ai-factory/infra-controller/issues/5403),
following the IPv6 URL fixes in [#6071](https://github.com/dsx-ai-factory/infra-controller/pull/6071)
and literal-address exchanges in [#6604](https://github.com/dsx-ai-factory/infra-controller/pull/6604).

## Prerequisites

- A Linux Docker Engine with IPv6 networking and support for
  `docker network create --ipv4=false`. Run inside a local Linux VM on macOS:
  the driver connects directly to container addresses when checking readiness.
- Python 3 and OpenSSL on that Linux host.
- An existing native repository build image containing the pinned Rust toolchain,
  PostgreSQL 15, Vault, and the other Core build prerequisites. The repository's
  [ARM64](../../docker/Dockerfile.build-container-aarch64) and
  [AMD64](../../docker/Dockerfile.build-container-x86_64) build recipes supply these.
- The ranges `172.30.54.0/24` and `fd00:5403::/64` must be unused by other Docker
  networks and must not overlap routes needed by the host. Docker rejects an
  overlapping Docker allocation; the driver does not modify existing networks.

The driver creates its own named networks and containers. It publishes no host
ports and uses disposable credentials. It attempts to remove its containers and
networks on completion or a handled failure. A single SIGINT/SIGTERM/SIGHUP
starts cleanup; repeated signals can interrupt it. Cleanup is best effort:
Docker errors or interruption can leave resources named `nico-dependency-*`.
Inspect their identities before removing them. The test image and Cargo caches
are retained for reuse.

## Build and run

From the repository root, substitute the name of the native build image if it
differs from `build-container-localdev`:

```bash
docker build \
  -f dev/deployment/dependency-network/Dockerfile \
  --build-arg BUILD_IMAGE=build-container-localdev \
  --build-arg VERSION="$(git describe --tags --first-parent --always --long)" \
  --build-arg CI_COMMIT_SHORT_SHA="$(git rev-parse --short=8 HEAD)" \
  --build-arg SOURCE_REVISION="$(git rev-parse HEAD)" \
  -t nico-dependency-tests .

python3 dev/deployment/dependency-network/qualify.py \
  --test-image nico-dependency-tests \
  --output /tmp/nico-dependency-results
```

Use a new output directory for each run. The image contains the working tree's
test code; its revision label records `HEAD`. Retain the diff as well when
qualifying uncommitted changes. The Dockerfile-specific context allowlist excludes
local environment files and includes the repository's public test certificates.

The build compiles the `carbide-api` and `carbide-secrets` library test binaries.
The driver invokes one named test per service and verifies that it ran. The Vault
and external-collector tests are ignored in ordinary test runs because they
require these disposable services. Existing literal-address tests remain part of
the ordinary suites.

## Cases and evidence

| Case | Endpoint and listener contract |
| --- | --- |
| IPv4 literal | IPv4 listener; direct IPv4 endpoint. |
| IPv6 literal | IPv6 listener; bracketed IPv6 URL. |
| AAAA only | DNS name has only an AAAA answer; IPv6 listener. |
| IPv6-only network | AAAA answers; clients query the IPv6 DNS server directly; Docker IPv4 networking disabled. |
| Dual stack | A and AAAA answers; both listeners available. |
| IPv4 refused | A and AAAA answers; only the IPv6 listener available. |
| IPv6 refused | A and AAAA answers; only the IPv4 listener available. |
| Missing DNS | No record for the requested names; all three exchanges must fail. |

PostgreSQL uses the production URL parser, authenticates with SCRAM over verified
TLS, and executes a query. Its server-reported connection family must match the
case when only one family is usable; the driver also requires its query-completion
marker, so a skipped test cannot pass qualification. Vault uses the production
client with TLS verification to create, read, delete, and verify absence of a test credential;
the server audit records establish the client address family. The production
Core OTLP exporter sends a uniquely named span over gRPC, and the driver requires
that span in the real collector's output. The missing-DNS case requires a
name-resolution error from each client, corresponding DNS queries, and no
credential requests or exported span.

The IPv6-only case mounts a resolver configuration naming the IPv6 DNS server,
and verifies IPv6 query peers in CoreDNS logs. The other hostname cases use
Docker's embedded DNS forwarder. Every case records each client's `resolv.conf`.
PostgreSQL's verified TLS exchange qualifies the URL parser and a migration-style
`verify-full` connection; Core's runtime pool instead enforces `require` when
TLS is configured and enforcement has not been disabled.

The evidence directory retains image identity, source revision, tool versions,
per-case network configuration, test output, CoreDNS queries, Vault audit records,
collector output, and a JSON result summary. These files contain disposable test
data. Generated CA private keys remain in temporary fixture storage and are
removed after the run.

## Qualification boundary

This checks production client helpers and actual service protocols. It does not
start Core through Helm, exercise Kubernetes Vault authentication, validate
PostgreSQL HA failover, or qualify every NICo binary. The existing deployment URL
construction checks and a site-specific rollout remain separate evidence.

The two refused-family cases establish that exchanges can succeed through the
remaining family. They do not establish which address was tried first, a fixed
family preference, or bounded fallback through a firewall that silently drops
packets. DNS changes after a connection is established are also outside these
cases. OTLP is qualified over plaintext gRPC within the isolated test network;
collector TLS and exporter retry after a collector restart are separate contracts.
