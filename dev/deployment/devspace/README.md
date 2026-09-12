# Local Development with DevSpace

You can use [DevSpace](https://www.devspace.sh) to deploy the complete local infra-controller stack. The deployment connects the REST services to the real Core gRPC API, while `machine-a-tron` supplies the mock hosts.

The process is broken into two steps:

1. Bootstrap Kubernetes prerequisites. (This only needs to be done once per cluster.)
2. Run `devspace deploy` to deploy code from this repo

The intent is that the app deploy path stays the same whether the prerequisites are:

- installed by the provided bootstrap script, or
- brought by the developer from elsewhere.

## Prerequisites Bootstrap

The bootstrap script operates on the current Kubernetes context and does not require a particular Kubernetes distribution. The provided full-stack deploy path uses kind-specific hooks to load locally built images into contexts named `kind-<cluster>`.

Run:

```bash
dev/deployment/devspace/bootstrap-prereqs.sh
```

By default this script assumes an empty cluster and will idempotently:

- install `cert-manager`
- create a local cert-manager issuer
- deploy a simple PostgreSQL instance
- deploy a simple Vault dev server
- configure Vault mounts and a local PKI role
- add the Vault PKI public CA to the generated Core admin-client trust bundle
- create a separate REST database in the local PostgreSQL instance
- deploy Temporal and create its `cloud` and `site` namespaces
- deploy the local Keycloak realm
- share the Core CA with REST so the site agent can use mTLS with Core
- create the Secrets and ConfigMaps that the Helm chart expects
- write [`values.generated.yaml`](values.generated.yaml) for the app deploy step

It is safe to re-run. It uses `helm upgrade --install`, `kubectl apply`, and Vault checks before writing mounts/roles/secrets.

The bootstrap script is responsible for cluster-facing dependencies and generated wiring only. The repo deploy step does not install PostgreSQL, Vault, cert-manager, Temporal, or Keycloak.

### Bring Your Own

You can skip the managed local services and still use the script to create only the chart wiring.

Examples:

```bash
LOCAL_DEV_INSTALL_POSTGRES=0 \
LOCAL_DEV_INSTALL_REST_PREREQS=0 \
LOCAL_DEV_POSTGRES_HOST=my-postgres.postgres.svc.cluster.local \
LOCAL_DEV_POSTGRES_PORT=5432 \
LOCAL_DEV_POSTGRES_DB=nico \
LOCAL_DEV_POSTGRES_USER=nico \
LOCAL_DEV_POSTGRES_PASSWORD=secret \
dev/deployment/devspace/bootstrap-prereqs.sh
```

```bash
LOCAL_DEV_INSTALL_VAULT=0 \
LOCAL_DEV_VAULT_ADDR=https://vault.example.internal:8200 \
LOCAL_DEV_VAULT_TOKEN=... \
LOCAL_DEV_VAULT_KV_MOUNT=secrets \
LOCAL_DEV_VAULT_PKI_MOUNT=certs \
LOCAL_DEV_VAULT_AUTH_MODE=root-token \
LOCAL_DEV_VAULT_ADMIN_CA_FILE=/path/to/vault-pki-ca.pem \
dev/deployment/devspace/bootstrap-prereqs.sh
```

`LOCAL_DEV_VAULT_ADMIN_CA_FILE` is optional when
`LOCAL_DEV_INSTALL_VAULT=0`. When set, it must name a readable regular file
containing only one or more valid X.509 certificates as bare PEM `CERTIFICATE`
blocks and whitespace. The public certificates are copied to
`nico-api.siteConfig.adminRootCertPem` in `values.generated.yaml`; private keys
and PEM metadata are rejected. When omitted for an external Vault, the
generated values do not set `adminRootCertPem`.

When the bootstrap script manages the local Vault, it reads the public CA
directly from `LOCAL_DEV_VAULT_PKI_MOUNT`. Setting
`LOCAL_DEV_VAULT_ADMIN_CA_FILE` overrides that CA.

```bash
LOCAL_DEV_INSTALL_CERT_MANAGER=0 \
LOCAL_DEV_INSTALL_LOCAL_ISSUER=0 \
LOCAL_DEV_INSTALL_REST_PREREQS=0 \
LOCAL_DEV_CERT_ISSUER_KIND=ClusterIssuer \
LOCAL_DEV_CERT_ISSUER_NAME=my-existing-issuer \
LOCAL_DEV_CERT_ISSUER_GROUP=cert-manager.io \
dev/deployment/devspace/bootstrap-prereqs.sh
```

Important:

- The script writes the generated Helm values file from these settings.
- The generated values trust the configured Vault PKI CA for authenticated
  Core admin-client operations when the bootstrap script manages local Vault
  or `LOCAL_DEV_VAULT_ADMIN_CA_FILE` is set.
- For local Vault, the app uses root-token auth by setting `automountServiceAccountToken: false`.
- For external Vault, either keep `LOCAL_DEV_VAULT_AUTH_MODE=root-token` or supply your own compatible auth setup.
- `LOCAL_DEV_INSTALL_TEMPORAL=0` and `LOCAL_DEV_INSTALL_KEYCLOAK=0` skip those managed services.
- `LOCAL_DEV_INSTALL_REST_PREREQS=0` preserves the Core-only bootstrap behavior.
- A full-stack deployment requires the `nico_rest`, `keycloak`, `temporal`, and `temporal_visibility` databases and roles when the local PostgreSQL installation is skipped. The REST API, workflow, and migration components use the absolute `postgres.postgres.svc.cluster.local.` Service DNS name. The trailing dot prevents the pod resolver from appending search domains while allowing Kubernetes to update the Service address normally. A nondefault PostgreSQL host is supported only by the Core-only path.
- The Core and REST services share one PostgreSQL server but use separate `nico` and `nico_rest` databases because both schemas contain tables such as `machines` and `instances`.

## Build And Deploy

Once the prerequisites are ready, run:

```bash
devspace deploy
```

DevSpace will:

- compile the shared Core binaries once with [`Dockerfile.core-artifacts`](Dockerfile.core-artifacts), then build the local runtime images from [`Dockerfile.api`](Dockerfile.api), [`Dockerfile.bmc-proxy`](Dockerfile.bmc-proxy), and [`Dockerfile.machine-a-tron`](Dockerfile.machine-a-tron); the `full` profile also builds [`Dockerfile.pxe`](Dockerfile.pxe)
- build the REST API, workflow, site-manager, site-agent, database migration, certificate-manager, and MCP images from [`rest-api/docker/local`](../../../rest-api/docker/local)
- deploy the Helm chart in [`helm/`](../../../helm) (including `nico-machine-a-tron`)
- deploy the REST umbrella, site-agent, and MCP charts in [`helm/rest`](../../../helm/rest)
- inject the built image names and DevSpace-generated tags into both deployments at runtime
- register a local REST site, configure its Temporal namespace, and confirm that the site agent establishes a Core gRPC connection

The image builds are configured in [`devspace.yaml`](../../../devspace.yaml). DevSpace always invokes the native [`dev/docker/Dockerfile.build-container-x86_64`](../../../dev/docker/Dockerfile.build-container-x86_64) or [`dev/docker/Dockerfile.build-container-aarch64`](../../../dev/docker/Dockerfile.build-container-aarch64) build so Docker notices architecture and Dockerfile changes while reusing unchanged layers from its cache. In the first build stage, a single shared builder compiles the API, admin CLI, BMC proxy, and machine-a-tron binaries while the REST images build in parallel. The builder exports those binaries to the local `nico-devspace-core-artifacts` image. In the second stage, the three Core runtime Dockerfiles copy their binaries from that image in parallel and add only their distinct runtime packages and assets. The `full` profile separately compiles and packages PXE while sharing the same BuildKit Cargo caches. DevSpace always invokes the enabled runtime builds because its custom-build change cache can outlive the corresponding local Docker images; Docker still reuses unchanged layers. BuildKit cache mounts are used for Cargo registry, Cargo git checkouts, and Cargo target output so rebuilds stay fast without copying host build artifacts into the image.

Host setup preloads PostgreSQL 14.5 for the DevSpace REST migration wait container. It also aliases that cached image as 14.4 inside the kind node for the standalone REST local deployment path, avoiding a second PostgreSQL image pull.

After deploying, [`setup-devspace-on-host.sh`](setup-devspace-on-host.sh)
checks PostgreSQL, every Temporal server deployment, and a functional Temporal
namespace query. It observes Temporal container restart counts while repeating
the namespace query and fails the setup if a container restarts during that
window. A successful process exit therefore means the workflow backend remained
usable through the final health check, not only that its Kubernetes readiness
probe passed earlier in the bootstrap.

The local Temporal server uses the absolute
`temporal-frontend.temporal.svc.cluster.local.` Service DNS name for its public
client. This avoids resolver search-domain expansion and works on both supported
host architectures.

The DevSpace images also use Dockerfile-specific ignore files. [`Dockerfile.core-artifacts.dockerignore`](Dockerfile.core-artifacts.dockerignore) provides the source needed by the shared artifacts, while [`Dockerfile.api.dockerignore`](Dockerfile.api.dockerignore), [`Dockerfile.bmc-proxy.dockerignore`](Dockerfile.bmc-proxy.dockerignore), [`Dockerfile.machine-a-tron.dockerignore`](Dockerfile.machine-a-tron.dockerignore), and [`Dockerfile.pxe.dockerignore`](Dockerfile.pxe.dockerignore) limit their build contexts. This keeps the top-level [`.dockerignore`](../../../.dockerignore) aligned with the main branch for CI and release builds.

The local REST Dockerfiles inherit BuildKit's target operating system and architecture. Native AMD64 hosts therefore produce AMD64 binaries, while native ARM64 hosts produce ARM64 binaries for the corresponding runtime images.

DevSpace watches the Rust workspace, toolchain metadata, and the runtime Dockerfiles to decide when the shared Core artifacts need rebuilding. It always runs the three second-stage Core runtime builds to guarantee their generated tags exist locally. On kind clusters, the pre-deploy hooks then load all Core and REST images into the cluster selected by the current kube context.

The `nico-machine-a-tron` Helm subchart configuration is in [`values.base.yaml`](values.base.yaml). The post-deploy setup resolves the `nico-machine-a-tron-mat-0-bmc-mock` Service ClusterIP and sets Core's runtime BMC proxy to that literal address. After allowing earlier requests to drain, it clears cached lockout-protection errors and refreshes existing host and DPU BMC endpoint records reported by machine-a-tron; endpoints not yet recorded on a clean install are left for normal discovery. This avoids hostname connection failures on affected ARM64 hosts and works unchanged on AMD64.

Common usage:

```bash
devspace deploy
devspace deploy -n nico-system
devspace deploy --skip-build -n nico-system
devspace deploy --force-build
```

To deploy NICo MCP, one CSC-local DSX Agent Gateway, and a local DSX
Exchange-compatible event bus, opt in with the `dsx-exchange` profile:

```bash
devspace deploy --profile dsx-exchange
```

The profile checks out NVIDIA/dsx-exchange `v2.9.1` at commit
`909f21c722b3f4eb6954a63ffbc3cb894685e3cd`, verifies that exact revision, and
uses the pinned DSX Agent Gateway chart from that checkout. The profile pins the
local Gateway API to its
[`v1.5.1` release](https://github.com/kubernetes-sigs/gateway-api/releases/tag/v1.5.1).
It is tested only with Agentgateway CRD `v1.4.1` and NATS Helm chart `2.12.6`.
The profile deploys one gateway in the CSC with NICo MCP as its directly
discovered backend. It does not enable the DSX sharding bridge. The gateway
validates the existing local Keycloak tokens and is available on NodePort
`30180`. Post-deploy verification also forwards it to
`http://localhost:18080/mcp`, authenticates with the local Keycloak token, and
requires direct NICo MCP tools without shard routing.

NATS remains an external upstream dependency: DevSpace consumes NATS Helm chart
`2.12.6` and its published runtime image instead of building NATS from the DSX
source checkout. The profile configures that single-node, unauthenticated NATS
service for NICo's managed-host MQTT publications to
`NICO/v1/machine/<machine-id>/state`. Current state is republished every 10
seconds. The profile does not enable the separate inbound
`nico-dsx-exchange-consumer`.

NICo MCP, the gateway release, NATS, and the publisher are
absent when the profile is not selected. The first profile build needs public
GitHub and OCI registry access to fetch the pinned DSX source and chart
dependencies. Later builds reuse the verified checkout under `.devspace/`.
The `dsx-exchange` and `core-only` profiles are incompatible because the
gateway requires the local REST API and Keycloak deployments.

### Full Profile

The `full` profile inherits `dsx-exchange` and also builds and deploys the
dedicated `nico-pxe` image and HTTP service:

```bash
devspace deploy -n nico-system --profile full --build-sequential
```

The default and `dsx-exchange` profiles leave `nico-pxe` disabled. The local
PXE image includes its request templates and Scout firmware scripts, but it
does not include OS boot artifacts. Configure `nico-pxe.bootArtifactContainers`
before using the profile for an actual host or DPU boot.

The post-deploy setup uses temporary port-forwards to register the site and verifies that machines from Core are visible through the REST API. To keep the REST API and Keycloak available on localhost after `devspace deploy` exits, run these in separate terminals:

```bash
kubectl -n nico-rest port-forward service/nico-rest-api 18388:8388
kubectl -n nico-rest port-forward service/keycloak 18082:8082
```

Then acquire a local token and list the machines discovered through `machine-a-tron`:

```bash
TOKEN=$(curl -fsS -X POST http://localhost:18082/realms/nico-dev/protocol/openid-connect/token \
  -H 'Content-Type: application/x-www-form-urlencoded' \
  -d 'client_id=nico-api' \
  -d 'client_secret=nico-local-secret' \
  -d 'grant_type=password' \
  -d 'username=admin@example.com' \
  -d 'password=adminpassword' | jq -r .access_token)
curl -fsS http://localhost:18388/v2/org/test-org/nico/machine \
  -H "Authorization: Bearer ${TOKEN}" | jq
```

To run the original Core-only deployment, skip the REST prerequisites during bootstrap and use the `core-only` profile:

```bash
LOCAL_DEV_INSTALL_REST_PREREQS=0 dev/deployment/devspace/bootstrap-prereqs.sh
devspace deploy --profile core-only
```

## Manual Equivalent

If you want to understand what DevSpace is doing for the runtime images, the configured build is effectively:

```bash
case "$(uname -m)" in
  x86_64) build_arch=x86_64 ;;
  aarch64|arm64) build_arch=aarch64 ;;
  *) echo "Unsupported CPU architecture: $(uname -m)" >&2; exit 1 ;;
esac
docker build --pull=false -t build-container-localdev \
  -f "dev/docker/Dockerfile.build-container-${build_arch}" .
docker build --pull=false -t nico-devspace-core-artifacts \
  -f dev/deployment/devspace/Dockerfile.core-artifacts .
docker build -t "nico-api:<devspace-generated-tag>" -f dev/deployment/devspace/Dockerfile.api .
docker build -t "nico-bmc-proxy:<devspace-generated-tag>" -f dev/deployment/devspace/Dockerfile.bmc-proxy .
docker build -t "machine-a-tron:<devspace-generated-tag>" -f dev/deployment/devspace/Dockerfile.machine-a-tron .
# full profile only
docker build -t "nico-pxe:<devspace-generated-tag>" -f dev/deployment/devspace/Dockerfile.pxe .
```

DevSpace then deploys the Helm chart with:

- the built `nico-api` image wired into `global.image.repository` and `global.image.tag`
- the built `nico-bmc-proxy` image wired into the `nico-bmc-proxy` chart values
- the built `machine-a-tron` image wired into the `nico-machine-a-tron` chart values
- with `full`, the built `nico-pxe` image wired into the `nico-pxe` chart values
- certificate issuer settings from the DevSpace environment variables

The REST images are built from the existing `rest-api/docker/local` Dockerfiles and are passed to the three existing REST Helm charts with the same generated tag.

## Resetting the local environment

Once deployed, the `nico-api` container will run and initialize its database, and the `machine-a-tron` container will run a set of mock machines, which will be discovered and ingested into the database, and run through the state machine until they reach a Ready state.

Reset the complete local environment by running:

```bash
devspace purge -n nico-system
```

When the current context is `kind-<cluster>`, the purge pipeline deletes and recreates that kind cluster with the same node image, then bootstraps clean prerequisites. This removes all Kubernetes state, including the Core and REST databases, Temporal namespaces and history, Vault data, Keycloak data, certificates, site registration, Helm releases (including machine-a-tron), CRDs, and persistent volumes.

The local REST migration hook uses the same PostgreSQL `14.5-alpine` image as the bootstrapped database, so the freshly pulled image is reused after cluster recreation.

On any other Kubernetes context, the pipeline delegates to DevSpace's default purge behavior. It removes the deployments managed by this project without replacing the cluster or reinstalling separately managed prerequisites.

The host Docker images, BuildKit cache, and `.devspace` image metadata are outside the kind node and remain available. Redeploy the last built images without rebuilding them:

```bash
devspace deploy --skip-build -n nico-system
```

The pre-deploy hooks load the cached Core and REST images from the host Docker store into the new kind node. Omit `--skip-build` when the source or image definitions have changed since the last build.

To clear only the Core `nico` database, run the nuke-postgres.sh helper script:

```bash
dev/deployment/devspace/nuke-postgres.sh
```

This helper does not reset the REST, Keycloak, or Temporal databases, the REST site registration, or Temporal namespaces. After resetting Core state, deploy again with:

```bash
devspace deploy -n nico-system
```

## Files

- [`prepare-ubuntu-host-for-dev.sh`](prepare-ubuntu-host-for-dev.sh)
- [`setup-devspace-on-host.sh`](setup-devspace-on-host.sh)
- [`reset-devspace-on-host.sh`](reset-devspace-on-host.sh)
- [`bootstrap-prereqs.sh`](bootstrap-prereqs.sh)
- [`reset-kind-cluster.sh`](reset-kind-cluster.sh)
- [`setup-rest-integration.sh`](setup-rest-integration.sh)
- [`devspace.yaml`](../../../devspace.yaml)
- [`values.base.yaml`](values.base.yaml)
- [`values.generated.yaml`](values.generated.yaml)
- [`nuke-postgres.sh`](nuke-postgres.sh)
