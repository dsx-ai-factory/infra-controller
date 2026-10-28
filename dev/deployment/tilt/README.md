# Local development with Tilt

Tilt runs the complete local NICo development stack from the repository's Helm
charts and Kubernetes sources.

> [!WARNING]
> This workflow is intended only for local development and is
> provided as-is. Use it at your own risk.

## Requirements

- Docker connected to a Linux AMD64 or ARM64 daemon, with BuildKit available
- Tilt
- Helm 3 or Helm 4, plus Python 3 for Tilt's `helm_resource` extension
- OpenSSL for validating the local Vault public CA certificate
- `kubectl` connected to a native kind cluster; the setup helper below requires
  kind 0.32.0 or newer and `jq`

The optional [native tool installer](../install-tools.md) supplies tested kind,
kubectl, Helm and Tilt versions:

```bash
python3 dev/deployment/install-tools.py
export PATH="$HOME/.local/bin:$PATH"
```

On Ubuntu 22.04+ ARM64 or AMD64, use the [host setup helper](../setup-host.sh)
from an existing trusted checkout to install host packages, Docker when absent,
and those tools:

```bash
bash dev/deployment/setup-host.sh --user "$USER"
```

It invokes sudo when needed and requires an existing non-root developer account.
When invoked as root, specify `--user USER` or retain `SUDO_USER`. Existing Docker
must be running and include containerd and Buildx; incomplete installations fail
before package changes. The helper adds Docker group access; start a new login
session afterward. It does not create a cluster, deploy the stack, or alter shell
profiles, huge pages, watcher limits or existing Docker storage. `--help` lists
the options without changing the host.

The Tiltfile refuses to load unless the active Kubernetes context starts with
`kind-`. This is stricter than Tilt's built-in local-cluster safety check and
prevents this development stack from targeting another cluster type.

## Create the development cluster

Start a local Docker runtime and select its Docker context. Run the shared
cluster helper with Docker, kind, `kubectl` and `jq` on `PATH`. The runtime must
provide Linux AMD64 or ARM64 containers through a local Docker Unix socket.

From the repository root:

```bash
bash dev/deployment/setup-kind.sh --name nico-tilt
export KUBECONFIG="$HOME/.kube/kind-nico-tilt.yaml"
kubectl --context kind-nico-tilt get nodes
```

Select an unused cluster for Tilt.

For an isolated dual-stack cluster, select a different name:

```bash
bash dev/deployment/setup-kind.sh --name nico-dual --ip-family dual
export KUBECONFIG="$HOME/.kube/kind-nico-dual.yaml"
kubectl --context kind-nico-dual get nodes -o wide
```

Both architectures use the same helper and configuration. Dual stack configures
Kubernetes pod/service networking and requires IPv6 support in the container
runtime's Linux kernel and Docker network, but not Internet IPv6.

The helper accepts these options; repeated options use the last value:

| Option | Default | Contract |
| --- | --- | --- |
| `--name NAME` | `nico-tilt` | Lowercase DNS label, 1–49 characters. Selects one cluster with one `NAME-control-plane` node. |
| `--ip-family FAMILY` | `ipv4` | `ipv4` or `dual`. An existing cluster's pod CIDRs must match. IPv6-only clusters are not accepted by this helper. |
| `--image IMAGE` | The digest-pinned Kubernetes 1.36.1 node image from the [kind 0.32.0 release](https://github.com/kubernetes-sigs/kind/releases/tag/v0.32.0) | Used only when creating a cluster; existing nodes are never upgraded. Custom images must work with the installed kind version and selected daemon architecture. |
| `--kubeconfig PATH` | `$HOME/.kube/kind-NAME.yaml` | Exported after node readiness, architecture and pod-family validation. Kind merges an existing file and selects the cluster context in that file. Relative paths resolve from the working directory. |
| `--help`, `-h` | — | Print usage and exit without host changes. |

Missing values, invalid names/families and unknown options fail before contacting
Docker. The helper requires kind 0.32.0 or newer for the pinned node image. It
captures the active Docker context, requires a local Unix socket and a Linux
AMD64/ARM64 daemon, and forces kind's Docker provider. Node images use the daemon
architecture. On Linux, the client and daemon architectures must also match.
The helper does not detect runtime hardware emulation. SSH/TCP
Docker endpoints are rejected because this helper does not arrange remote API
access; run it on that Docker host instead.
A `DOCKER_HOST` Unix socket is honored with Docker's default context.

Validation uses a private temporary kubeconfig with explicit context selection;
inherited `KUBECONFIG` is ignored. Your normal kubeconfig and current context
remain unchanged unless you explicitly select that file with `--kubeconfig`.
New clusters publish their API on `127.0.0.1`, on a port chosen by kind. Run Tilt
from the same client, using the exported kubeconfig and the same
Docker context.

Setup reuses compatible clusters without pulling a node image, restarting nodes
or changing installed workloads. It rejects multiple-node clusters and mismatched
pod families or architectures. It passes `--wait 180s` to kind and
`--timeout=180s` to the node readiness check; these do not impose an overall
timeout on image downloads or setup. Failure returns nonzero, removes temporary
files and lets kind clean up a failed creation. A cluster that was created
successfully but fails subsequent validation remains available for diagnosis.
The helper does not delete/recreate preexisting clusters or repair incompatible
settings. Choose another name or explicitly remove an unused
cluster through kind after accounting for its workloads and data.

## Start

From the repository root, run:

```bash
tilt up -f dev/deployment/tilt/Tiltfile
```

The default stack includes:

- cert-manager
- the CloudNativePG operator and one PostgreSQL cluster
- Vault in local development mode
- NICo API, BMC proxy, DHCP server, machine-a-tron, and mat-k8s-controller

Enable REST with:

```bash
tilt up -f dev/deployment/tilt/Tiltfile -- --rest=true
```

This adds:

- Temporal and its local namespaces
- Keycloak and the `nico-dev` realm
- NICo REST API, database migrations, certificate manager, site manager,
  workflow workers, and site agent
- automatic local site registration

Enable MCP separately with `--mcp=true`; it also enables REST.

Tilt forwards these endpoints:

| Service | URL |
| --- | --- |
| NICo Web UI | `https://localhost:1079/admin/` |
| Machine-A-Tron UI | `https://localhost:1266/` |
| REST API | `http://localhost:18388` |
| Keycloak | `http://localhost:18082` |
| MCP | `http://localhost:18080/mcp` |
| Temporal UI | `http://localhost:18233` |
| Grafana | `http://localhost:13000` |
| Prometheus | `http://localhost:19090` |

## Core prerequisites

The `nico-core-prereqs` release uses the shared `helm-prereqs` chart and
[local development profile](../../../helm-prereqs/values/local-development.yaml).
It owns Core's CA, Issuer, SSH host key, Vault Secrets and connection ConfigMaps.
Tilt supplies the existing local CA and OpenSSH key fixture with Helm's
`--set-file`; the same profile and files work with
[direct Helm](../../../helm-prereqs/README.md#local-core-prerequisites).
With REST enabled, the same release also owns the REST trust resources described
below. Vault and PostgreSQL installation, database provisioning and site
registration remain separate steps. Use a fresh development cluster when switching from
Tilt-created Core prerequisites; Helm does not adopt their existing objects.

## MAT networking

Helm owns the local network Services. The DHCP chart pins `nico-dhcp` to
`10.96.0.20` through `service.clusterIP`; the local values set Kea's
`config.kea.serverIdentifier` to the same address for unicast renewals. An empty
`service.clusterIP` retains Kubernetes address allocation for other chart users.
An explicit address must be IPv4, available in the cluster's ServiceCIDR, and is
immutable after creation. The chart does not infer the Kea identifier from it.

The MAT chart uses `dhcpRelay.baseIP: 10.96.0.21`, `listenPort: 67` and
`serverAddress: 10.96.0.20:67` for both its relay Service and the placeholders
in the local TOML configuration. Its optional `service.ssh.headless: true`
provides direct pod DNS for console ports reported by Redfish. Both settings
are ordinary chart values; Tilt no longer creates separate network Services.
The same values render these resources with Helm without evaluating a Tiltfile.
This does not replace the remaining Tilt prerequisite and site-registration steps.

Use a fresh development cluster when switching from a checkout that created
these Services directly in Tilt. Helm will reject a relay or console Service
owned outside its release, and an existing DHCP Service may already have a
different immutable ClusterIP. There is no in-place ownership or address migration
for these Services. Keep any cluster with needed lab data until that data
has been accounted for; the cluster setup helper does not recreate it.

## Site registration

Use a fresh development cluster when switching from the older Tilt setup that
rendered a placeholder registration Secret. Reloading that setup can overwrite
live credentials; deleting the Secret does not recover an already consumed OTP.
The registration-first path starts with an absent Secret or complete existing
credentials for the reused site. For a placeholder created by another legacy
setup, complete that setup's documented `site-agent` bootstrap before switching
to Secret identity.

With REST enabled, `rest-integration` runs the shared `register-site` command
before deploying the Site Agent. It uses the local REST API to create or reuse
`local-dev-site` for the current infrastructure provider in `test-org`, ensures
its Temporal namespace exists, and creates the `site-registration` Secret.
Registration owns that Secret; the Site Agent may rotate its OTP. Tilt does not
render a placeholder or patch the Helm-owned ConfigMap.

The Site Agent chart's `siteIdentityFromRegistrationSecret` defaults to `false`;
Tilt enables it. Both `CLUSTER_ID` and `TEMPORAL_SUBSCRIBE_NAMESPACE` then read
`site-uuid` from `secrets.siteRegistration`, overriding `envConfig` for those
fields. The Secret and key must exist before the pod can start. This mode rejects
`bootstrap.enabled=true`; registration runs through the API before Helm deploys
the agent. Other chart consumers retain the ConfigMap identity path by default.

For an already deployed local REST stack, select its kubeconfig and keep API and
Keycloak port-forwards running, then use the same command directly:

```bash
NAMESPACE=nico-rest API_URL=http://localhost:18388 KEYCLOAK_URL=http://localhost:18082 \
  bash rest-api/scripts/setup-local.sh register-site
```

`NAMESPACE` defaults to `nico-rest`; `API_URL` and `KEYCLOAK_URL` default to
`http://localhost:8388` and `http://localhost:8082`. `ORG` defaults to `test-org`.
The command requires Bash, kubectl, curl, jq and base64, the development
`nico-dev` realm and its fixture administrator, a ready REST API/site-manager,
and Temporal in namespace `temporal`. It uses the selected kubectl context.
It does not restart the API or agent (`RESTART_API` and `WAIT_FOR_SITE_AGENT`
apply to the legacy `site-agent` command). The legacy command also configures
and restarts its already installed agent.

Rerunning registration preserves a complete matching Secret, including a rotated OTP;
it never rebinds an existing real registration to another site. Completeness requires
nonempty `site-uuid`, `otp`, `creds-url` and `cacert` data. An incomplete Secret is
rejected even if the API still exposes a token; restore the complete credential
set for the same site before rerunning. Incomplete credentials, failed registration Secret or site-list reads,
ambiguous names and an absent match on a full 100-site page fail instead of
creating a new binding. This helper is for the bounded local fixture, not bulk
site administration. If a registered site's credentials are lost and the API no
longer returns its token, restore the saved Secret through the site's recovery
procedure; rerunning cannot recover a consumed token. Removing the agent's Helm
release leaves its registration and API site intact. Reinstall with the same
Secret. Changing its site ID requires a separately managed re-registration and
pod replacement; this command does not change an existing binding.

## Development Keycloak

With REST or MCP enabled, the `keycloak` Helm release owns the Deployment,
ClusterIP and NodePort Services, realm ConfigMap and database credential Secret.
The [shared development chart](../../../helm/nico-dev-keycloak/README.md) supplies
the pinned multi-architecture image. Optional `development.keycloak.image` overrides it;
`development.keycloak.databaseUrl` and `development.localConfig.database.keycloak` supply the
existing database connection. Tilt waits for database provisioning before
installing Keycloak and for Keycloak readiness before starting the REST API.

The canonical `nico-dev` realm JSON is shared with the retained Kustomize
workflow. Import seeds an absent realm; it does not update a realm already in
PostgreSQL. Use Keycloak's administration API or console to change an existing
realm. Reconciliation or recreating the Keycloak workload preserves users,
clients and identity data in the separately managed database. Database credential
changes roll the workload and require matching changes to the database role.

Use a fresh development cluster when switching from raw Tilt Keycloak resources;
Helm does not adopt their ownership. Removing the release interrupts login and
removes its Kubernetes objects, but does not remove the database. Reinstall with
the same database inputs to recover the existing realm. The reference deployment's
`nico` realm and its setup script remain separate from this `nico-dev` workflow.

## REST trust prerequisites

With `--rest=true` or `--mcp=true`, `nico-core-prereqs` also owns the
`ca-signing-secret` Secrets in `cert-manager` and the REST namespace, the
`nico-rest-ca-issuer` ClusterIssuer, and the three Temporal server Certificates.
They use the same supplied local CA as Core. Tilt forwards the REST and Temporal
namespace settings from `development.localConfig` and the issuer name from
`development.rest.global.certificate.issuerRef`; Tilt requires
`kind: ClusterIssuer` and accepts only `group: cert-manager.io` (also the default
when `group` is omitted). Incompatible references fail before deployment.
The chart's [REST trust settings](../../../helm-prereqs/README.md#local-rest-trust)
also work with direct Helm. Namespace creation precedes the release, and Temporal
startup waits for all three Certificates to become Ready.

Use a fresh development cluster when switching from raw Tilt CA Secrets and
Temporal Certificates; Helm does not adopt them. Reconciliation with unchanged
CA inputs preserves trust and certificate identities. Disabling REST updates the
prerequisite release to remove its REST trust resources, so stop REST consumers
before switching to Core-only mode. Uninstalling the prerequisite release removes
both Core and REST trust resources. Preserve its CA and SSH key inputs for
restoration against retained application data; neither removal nor restoration
registers a new site or changes database ownership.

## REST common prerequisites

With `--rest=true` or `--mcp=true`, the `nico-rest-common` Helm release owns
`db-creds`, `keycloak-client-secret`, `temporal-encryption-key`,
`image-pull-secret` and the `temporal-client-cloud-cert` Certificate in the REST
namespace. Tilt supplies the database, Keycloak and Temporal inputs from
`development.localConfig` and the issuer from `development.rest.global.certificate.issuerRef`.
The shared [chart](../../../helm/rest/README.md#installing-common-resources-separately)
can also be installed directly.

Tilt sets `secrets.migrationHooks=false`: these are ordinary Helm resources
installed before the migration Job. The `rest-certificate-ready` resource waits
for certificate issuance before starting API and workflow services. The chart
leaves CA signing, database provisioning and site registration to their existing
setup steps.

Use a fresh development cluster when switching from a checkout where Tilt
created these resources directly. Helm does not automatically adopt those
objects. On a stack whose common resources were installed by the
`nico-rest-common` Helm release, reconcile them from the repository root with:

```bash
helm upgrade nico-rest-common helm/rest/nico-rest/charts/nico-rest-common \
  --namespace nico-rest --reuse-values --wait --timeout 5m
kubectl -n nico-rest wait --for=condition=Ready \
  certificate/temporal-client-cloud-cert --timeout=5m
```

Select the stack's kubeconfig first; use its REST namespace if it differs from
`nico-rest`. Reconciliation keeps the supplied credential values and site
identity. Uninstalling `nico-rest-common` removes its four Secrets and Certificate
and interrupts consumers that need them. Preserve the release's credential
values before removal; restoring against retained data requires the same
Temporal encryption key and database credentials. This release does not own
PostgreSQL storage or the site-registration Secret.

## Verify stack readiness

The `stack-ready` resource runs after Core and MAT start. It requires a nonempty
set of distinct MAT host identities matching Core, every host in `Ready` or
`Assigned/Ready`, and a responsive Scout stream for every idle `Ready` host.
Switches, DPUs and power shelves do not count as hosts. Assigned hosts run a
tenant OS and do not require Scout.

With `--rest=true` or `--mcp=true`, verification also waits for matching site
effective agent and registration identities, an online `Registered` site, the same usable host identities in
REST, and a successful final-page completion logged after verification started,
with the expected nonzero host count. This can take several minutes because REST
inventory is periodic. The completion record must contain structured page
metadata; rebuild the REST workflow image from this checkout when upgrading an
existing stack. Older worker images without that metadata cannot pass this
check. This receipt proves final-page processing, not atomic completion of every
page in an inventory batch.
A failure makes the resource fail, including in `tilt ci`; Kubernetes pod
readiness alone is insufficient. Verification checks discovery and inventory,
not instance allocation or tenant network connectivity.

While Tilt is running, rerun the same check from the repository root using the
cluster kubeconfig selected above:

```bash
python3 dev/deployment/verify-stack.py
python3 dev/deployment/verify-stack.py --rest
```

The verifier uses Python 3, Bash, `kubectl`, `jq` and `curl`. It captures the active
Kubernetes context, or accepts `--context NAME`; Tilt passes its selected
`kind-` context explicitly. It uses the bundled Core admin CLI with a short-lived certificate
issued by the local Vault, without changing Core or Helm configuration during
verification. Tilt's `vault-admin-ca` prerequisite exports the public Vault CA to
an ignored `.local/CONTEXT/vault-admin-ca.json` file beside the Tiltfile. The Core
Helm release consumes it through `siteConfig.adminRootCertPem`; its local auth
configuration also recognizes the Vault issuer. The export contains no private
key. If the ephemeral Vault restarts with a new CA, rerun `vault-admin-ca` and
then update `nico-api` before rerunning readiness.
It polls every five seconds with a 900-second overall deadline, limits individual
commands to 45 seconds, and returns nonzero on failure. `--timeout SECONDS`
accepts 1–3600; repeated options use the last value. `--help` lists the options.

Defaults are `--core-namespace nico-system`,
`--mat-service nico-machine-a-tron-mat-0-bmc-mock`, and
`--rest-namespace nico-rest`. This check supports one MAT control Service at
HTTPS port 1266 and, with REST enabled, at most 100 hosts in `test-org` using the
local `nico-dev` Keycloak realm. `--api-url` defaults to
`http://localhost:18388` and `--keycloak-url` to `http://localhost:18082`;
REST checks require these port-forwards to remain available. REST namespace and
URL options are ignored without `--rest`. The verifier creates no port-forwards
and does not register sites, reset machines or repair failed services.

To rerun from Tilt, trigger `stack-ready` in the UI or use:

```bash
tilt trigger stack-ready
```

## NICo PostgreSQL dashboard

With `--observability=true`, Tilt installs the shared
[`nico-postgres-exporter` Helm chart](../../../helm/charts/nico-postgres-exporter/README.md)
as a separate release in `nico-system`. It builds the exporter image through the
shared Rust artifacts image and provisions **NICo / PostgreSQL** alongside
**CloudNativePG** in Grafana's **PostgreSQL** folder.

```bash
tilt up -f dev/deployment/tilt/Tiltfile -- --observability=true
```

Open [Grafana at localhost:13000](http://localhost:13000) and choose
**PostgreSQL → NICo / PostgreSQL**. The dashboard includes connection health and
a cumulative **Slow queries** table. See the chart README for collection behavior,
metric contracts, binary configuration, and installation with your own Grafana.

Tilt passes `development.observability.postgresExporter` as chart values, supplying the
image and the `postgresql` host, port, and database name from the shared
local prerequisite profile. Development
values use `database.credentialsSecret: nico-pg-cluster-app` and
`database.sslMode: disable`; the credentials Secret is in the release namespace.
For verified TLS, configure `database.sslMode` and `database.sslRootCertSecret`
under `development.observability.postgresExporter`. Tilt enables the ServiceMonitor and
places the dashboard ConfigMap in `observability`, waiting for Prometheus,
PostgreSQL, and local configuration before installation. Disabling observability
excludes the exporter release and dashboard.

With observability enabled, Tilt sets `pg_stat_statements.track: top` and
`pg_stat_statements.track_utility: off`, excluding utility statements that could
contain password literals from new query statistics. It also grants the application
owner `pg_read_all_stats` membership. CloudNativePG manages the
preload library and installs the extension in connectable databases when that
parameter is present; changing the preload configuration can restart PostgreSQL.
See [CloudNativePG's managed-extension contract](https://cloudnative-pg.io/docs/1.28/postgresql_conf/#enabling-pg_stat_statements).
The shared chart itself does not configure extensions or database permissions.
For external databases and existing statistics collected before disabling utility
tracking, follow the [database setup instructions](../../../helm/charts/nico-postgres-exporter/README.md#database-collection-and-dashboard).

The dashboard JSON now lives in the chart at
[`dashboards/nico-postgres.json`](../../../helm/charts/nico-postgres-exporter/dashboards/nico-postgres.json).
Tilt watches the chart directory; edits update the Helm-managed ConfigMap and
Grafana's existing sidecar loads them. Allow one collection and scrape interval
for fresh data. Chart-only installs can enable the optional umbrella dependency
or install this chart as a separate release; both are disabled by default.

If upgrading a Tilt environment from the direct Kubernetes exporter wiring,
remove the old Tilt-owned `nico-postgres-exporter` Deployment, Service, and
ServiceMonitor in `nico-system`, plus the old
`grafana-dashboards-nico-postgres` ConfigMap in `observability`, then retry the
exporter resource. Helm cannot adopt those existing objects. This is a one-time
migration; do not remove resources already owned by the new Helm release.

## Pod logs

Grafana Alloy collects stdout and stderr from every pod through the Kubernetes
API and sends the logs to the local Loki instance. Grafana has Loki provisioned
as its default data source and permits anonymous access because its Tilt
port-forward listens only on localhost.

Open **Explore** in Grafana and use a LogQL selector such as:

```text
{cluster="local-carbide"}
```

Narrow the results with the `namespace`, `workload`, `pod`, `container`, or
`app` labels. For example, machine-a-tron logs can be selected with:

```text
{namespace="nico-system", app="nico-machine-a-tron"}
```

Loki stores logs on a 2 GiB persistent volume and retains them for 24 hours.
The volume survives pod restarts but is removed with the local Kind cluster.

## API traces

With `--observability=true`, Tilt installs Tempo and enables `nico-api` trace
sampling with the chart's `tracing.enabled` value. The API exports OTLP/gRPC
traces to `http://tempo.observability.svc.cluster.local:4317` by default.
Set `nico-api.env.OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` in
[`values.yaml`](../stack/values.yaml) to use a different OTLP/gRPC URL. An empty or
omitted endpoint selects the local Tempo service when the profile is enabled.
The environment variable overrides `[tracing].otlp_endpoint` in the API's TOML
configuration; site TOML overrides the chart's generated `[tracing].enabled`.

Open **Explore** in Grafana, select **Tempo**, and search for service name
`carbide-api`. Tempo retains traces for 24 hours on a 2 GiB persistent volume.
The volume survives pod restarts and is removed with the local Kind cluster.
When enabling observability in an existing session, trigger the manual
`nico-api` resource in Tilt to apply its tracing configuration.

To export REST traces into the same Tempo instance, enable both profiles:

```bash
tilt up -f dev/deployment/tilt/Tiltfile -- --observability=true --rest=true
```

Tilt enables trace export for the REST API, cloud and site workers, site-agent,
cert-manager, and site-manager. Existing HTTP, Temporal, and Core gRPC
instrumentation carries W3C trace context from REST requests into `carbide-api`.
In Grafana Explore, search Tempo for `nico-rest-api`, `nico-rest-cloud-worker`,
`nico-rest-site-worker`, `nico-rest-site-agent`, `nico-rest-cert-manager`, or
`nico-rest-site-manager`. Requests that reach Core through Temporal should show
REST and `carbide-api` spans in the same trace.

Under `development.rest` in [shared values](../stack/values.yaml), configure `OTEL_*` overrides in
`api.extraEnv`, `certManager.extraEnv`, `siteManager.extraEnv`,
`workflow.extraEnv`, or `siteAgent.envConfig`. The workers also accept
`workflow.cloudWorker.extraEnv` and `workflow.siteWorker.extraEnv`, which take
precedence over shared worker environment settings. Tilt defaults the exporter
endpoint to `http://tempo.<observabilityNamespace>.svc.cluster.local:4317` and
the protocol to `grpc` only when neither the general nor traces-specific
variable has a non-empty value. The Go exporter gives traces-specific variables
precedence over general variables. Explicit `OTEL_SERVICE_NAME` values override
configured service names; an explicit `workflow.config.tracing.serviceName`
replaces the distinct worker defaults. Other services retain their existing
service-name defaults. Disabling observability adds no REST tracing settings;
explicit exporter configuration remains effective.

For an existing Tilt session, use `tilt args -- --observability=true --rest=true`,
then trigger `nico-api`, `nico-rest-api`, `nico-rest-workflow`,
`nico-rest-site-agent`, `nico-rest-cert-manager`, and `nico-rest-site-manager`
to apply the configuration to these manual resources. Once the services are
ready, trigger `rest-integration` to restore the registered site ID after the
site-agent chart reapplies its development placeholder.

## Pod metrics

Prometheus discovers ServiceMonitor and PodMonitor resources across the local
cluster. It also automatically scrapes every Kubernetes Service labeled
`app.kubernetes.io/metrics` whose service name ends in `-metrics` or whose port
is named `metrics`. This collects NICo service metrics without redeploying the
manual application resources. Grafana provisions Prometheus as a data source
alongside Loki.

Open **Explore** in Grafana, select the Prometheus data source, and start with:

```promql
up{namespace="nico-system"}
```

Prometheus stores metrics on a 2 GiB persistent volume and retains them for 24
hours. Its UI at `http://localhost:19090` also shows discovered endpoints under
**Status > Targets**.

## Dashboards

Tilt provisions the checked-in **NICo Core Health**, **NICo / Site Overview**,
**NICo / Site Explorer**, **NICo / Object Lifecycle**, and **NICo / API
Performance** dashboards into the **NICo** Grafana folder. The local Site
Explorer dashboard adapts the canonical Forge dashboard panels to the current
`carbide_*` metric names. Grafana's dashboard sidecar watches their labeled
ConfigMap, so changes to the dashboard JSON files are loaded without a manual
import or Grafana restart.

The Prometheus chart also publishes its standard Kubernetes dashboards for the
same sidecar to load. These cover cluster, namespace, workload, pod, node,
kubelet, API server, and persistent-volume metrics.

With the observability profile enabled, Tilt also enables PodMonitors for the
CloudNativePG operator and PostgreSQL cluster. The operator chart provisions the
**CloudNativePG** dashboard in Grafana's **PostgreSQL** folder, covering operator
and database metrics. Tilt places its dashboard ConfigMap in the observability
namespace and waits for Prometheus before installing the operator's PodMonitor.

If Tilt is already running, it reloads changes to the Tiltfile and values file
and updates the `cnpg-operator` and `nico-postgres` Helm releases automatically.
Enable the profile with `tilt args -- --observability=true` if needed. To retry
an update, use the resource's update button in Tilt or run:

```bash
tilt trigger cnpg-operator
tilt trigger nico-postgres
```

Grafana's sidecar loads the dashboard automatically. Allow time for the Helm
updates and a Prometheus scrape, then refresh Grafana; no manual dashboard
import is required.

Shared Kubernetes prerequisites are rendered from
[`nico-dev-prereqs`](../../../helm/nico-dev-prereqs/README.md). Tilt keeps resource
ordering, readiness checks, and apply ownership; it does not create a second Helm
release for previously applied namespaces, database objects, or credentials.

Deployment settings are in [shared values](../stack/values.yaml). NICo Core values are at
the document root, while the `development` section contains the prerequisite and REST
settings.

## Machine-a-tron BMC routing

Tilt runs machine-a-tron in controller mode. `mat-k8s-controller` polls the
machine-a-tron status endpoint and creates one Kubernetes Service per simulated
BMC, publishing the BMC address assigned by NICo as the Service's `externalIPs`.
NICo therefore connects directly to each simulated BMC instead of routing every
Redfish request through the shared machine-a-tron proxy.

The Tilt BMC underlay is `10.200.0.0/18`. BMC addresses are Service externalIPs,
which the apiserver neither allocates nor validates and for which kube-proxy
programs forwarding rules on every node, so this range must stay outside the
Kubernetes ServiceCIDR, the pod CIDR, the node network, and any network the
nodes or pods must otherwise reach. On Kind the first two default to
`10.96.0.0/16` and `10.244.0.0/16`. Refer to the chart's
[Requirements](../../../helm/charts/nico-machine-a-tron/README.md#requirements)
for the full contract.

The DPU OOB / switch NVOS underlay (`192.168.128.0/18`) has the same
constraint: the controller publishes each simulated NVLink switch's NVOS lease
as the externalIP of a `mat-nvos-*` Service, through which NICo reaches
machine-a-tron's hosted NMX-C mock on port 9370 and the switch's NVUE REST API
mock on port 443.

## Image builds

Tilt and `make images-dev` share the Core service recipes and build-context
ignore files in `dev/docker/Dockerfile.dev-services` and
`dev/docker/Dockerfile.dev-runtime`. The compiler-container Dockerfiles in
`dev/docker` are also shared with other build workflows. Tilt retains the
MAT controller recipe in this directory and uses the REST service Dockerfiles
under `rest-api`.

Both workflows select the native Linux AMD64 or ARM64 architecture from the
active Docker daemon. Other daemon platforms fail before builds start. The Docker
daemon and the kind cluster must use the same architecture.

For a standalone image build, use `make images-dev` from the repository root.
See [building one local development service](../../../docs/manuals/building_nico_containers.md#build-one-local-development-service)
for target selection, image names, prerequisites and the isolated DHCP image
runtime check. It builds into the selected Docker daemon without publishing
images or deploying Kubernetes resources.

The main stack runtime images derive from one digest-pinned Debian Bookworm slim base
with CA certificates. Service images install their additional runtime packages;
the compiler toolchains remain in build stages. DHCP includes both Kea DHCPv4
and DHCPv6 servers, using the version in `dev/docker/kea.version` to match the
compiler container. Its hook library is installed at
`/usr/lib/kea/hooks/libdhcp.so` on both architectures. Tilt values select that
path for DHCPv4; the DHCPv6 chart already uses it directly.

`Dockerfile.dev-services` has separate targets for API, BMC proxy, DHCP,
machine-a-tron, SSH console, the MAT protocol gateway and PostgreSQL exporter.
Each target builds and packages its own binary (or DHCP hook library), sharing
locked Cargo dependency and target caches separated by architecture. The Rust
build stages use `line-tables-only` debug information for development and release
profiles, retaining source-line
backtraces without full debugger type and variable information. The API image
contains a copy of nico-admin-cli for convenience.

The MAT controller and its gateway also use separate images in Tilt. Other chart
users keep the combined controller image unless they set
`mat-k8s-controller.gateway.image.repository` or `.tag`; each empty field falls
back to the corresponding controller image field, and both containers use the
controller image pull policy. REST API images contain only the API service
binary, without the debugging CLI.

Each deployable image has its own Tilt resource and rebuild control. Images build
once during startup, then wait for their resource's update button before
rebuilding. REST build contexts are restricted to the source directories copied
by that image's Dockerfile. Docker and Cargo or Go build caches are reused across
rebuilds. Repeated changed-source builds still retain historical layers until
Docker, Tilt, or the cluster removes them; this layout does not set storage
retention policies.

## Optional profiles

The `development.profiles` switches in [shared values](../stack/values.yaml) control REST, MCP,
DSX Exchange, observability, and DPF. REST brings Temporal and Keycloak, MCP
automatically brings REST, and DSX Exchange brings NATS. Observability manages
Loki, Tempo, Alloy, Prometheus, Grafana, and the NICo metrics monitors as one unit. All
optional profiles are disabled by default.

Changes to [shared values](../stack/values.yaml) reload the running Tilt session. For temporary
overrides, use `tilt args`. Start only Core with:

```bash
tilt args -- --rest=false --mcp=false
```

Run REST without MCP with:

```bash
tilt args -- --rest=true --mcp=false
```

Add DSX Exchange with:

```bash
tilt args -- --dsx-exchange=true
```

Enable the complete observability stack with:

```bash
tilt args -- --observability=true
```

Enable DPF-managed DPU initialization by starting the session with:

```bash
tilt up -f dev/deployment/tilt/Tiltfile -- --dpf=true
```

The DPF profile requires ephemeral Vault; combining `--dpf=true` with
`--persistent-vault=true` is rejected before deployment.

The DPF profile installs the DPF CRDs from
[`crates/dpf/crds`](../../../crates/dpf/crds), creates the
`dpf-operator-system` namespace, and builds and deploys the
[DPF simulator](../../k8s/dpf-sim-controller/README.md) in place of the real
DPF operator. NICo API starts with `[dpf] enabled = true` in its site
configuration, a mounted ServiceAccount token, and the chart's DPF RBAC, so
simulated machines run DPU initialization through the DPF provisioning flow
instead of the legacy iPXE path. Never enable this profile on a cluster that
also runs the real DPF operator: both would drive the same `DPU` resources.
Because NICo API's Vault client switches to Kubernetes ServiceAccount login
whenever a token is mounted, this profile extends the dev Vault's `postStart`
hook to configure the `kubernetes` auth method and bind the `nico-api`
ServiceAccount. Vault's StatefulSet only applies updates on pod deletion, so
when enabling the profile against a cluster whose Vault is already running,
delete the `nico-vault-0` pod once so the extended hook runs (dev-mode Vault
keeps no durable state). Trigger `vault-admin-ca` to export the new CA, wait for
it to finish, then retrigger `nico-api`.

Prefer a fresh `tilt up` over toggling `tilt args -- --dpf=true` in a running
session: dependency ordering applies only to first-time deploys, so a
mid-session toggle can redeploy NICo API before the namespace and CRDs exist,
leaving it failed until manually retriggered. The simulator covers initial DPU
provisioning only; DPF-driven reprovisioning on BFB or flavor changes stays
inert because no `DPFOperatorConfig` reports `Ready` in this profile.

Return to the values-file defaults with:

```bash
tilt args --clear
```

## Persistent local Vault

For a lab that needs to retain rotated BMC credentials, select persistent Vault
when first starting a **new** kind cluster:

```bash
tilt up -f dev/deployment/tilt/Tiltfile -- --persistent-vault=true
```

`--persistent-vault` defaults to `false`. The default uses an in-memory Vault
with fixture credentials. The persistent option uses the same Vault chart with
a 1 GiB `standard` PVC and file storage, adapted from
[the kind integration work in #7555](https://github.com/dsx-ai-factory/infra-controller/pull/7555).
Both modes use local HTTP without TLS and are restricted to kind contexts.
Changing the mode of an existing Vault, or starting ephemeral Vault over its
retained storage, is rejected. There is no in-place conversion.

The `vault-bootstrap` resource initializes one unseal share with threshold one,
stores it and the root token in the immutable `nico-vault-local-keys` Secret,
and creates the immutable `nico-vault-token` Secret consumed by Core. Both live
in `nico-system` and are tied to the data PVC's UID. The prerequisite chart
leaves that token Secret under bootstrap ownership in persistent mode.
Bootstrap sets the PKI mount maximum TTL to ten years before generating its
first CA. It preserves an existing CA and seeds default BMC/UEFI credentials only
when no version exists; rerunning it never resets a rotated or soft-deleted value.

Vault starts sealed after a pod restart. While Tilt is running, unseal it with:

```bash
tilt trigger vault-bootstrap
```

Without a running Tilt session, run the same operation directly (substitute the
selected kind context):

```bash
python3 dev/deployment/stack/vault-bootstrap.py --context kind-nico-tilt bootstrap
```

The helper accepts `--namespace` before the subcommand, defaulting to
`nico-system`. `bootstrap` requires an installed Vault and bound data PVC; it
fails on missing keys for an initialized Vault, foreign Secret ownership,
changed PVC identity, or an incompatible mount. Credentials travel through
stdin and captured output. If initialization succeeds but saving the keys
Secret fails, it reports a mode-0600 local recovery file containing those keys;
retain that file securely until the matching Secret has been recovered.
The helper prints the recovery-file path before starting initialization.
Initialization output streams directly into that file, so received output
survives a helper interruption or transport error. The system temporary
directory can be cleared on reboot; move a needed recovery file to secure,
durable storage promptly. Empty output cannot recover keys; bootstrap never reinitializes an
already initialized Vault.

To recover a complete initialization JSON file into a missing keys Secret, first
verify that it belongs to this Vault and that its original data PVC is still
present. Set the context, namespace and recovery file path, then recreate the
ownership metadata and immutable Secret:

```bash
vault_context=kind-nico-tilt
vault_namespace=nico-system
vault_keys_file=/path/to/nico-vault-init-recovery.json
vault_pvc_uid=$(kubectl --context "$vault_context" -n "$vault_namespace" \
  get pvc data-nico-vault-0 -o jsonpath='{.metadata.uid}')
kubectl --context "$vault_context" -n "$vault_namespace" create secret generic \
  nico-vault-local-keys --from-file="keys.json=$vault_keys_file"
kubectl --context "$vault_context" -n "$vault_namespace" label secret \
  nico-vault-local-keys app.kubernetes.io/managed-by=nico-local-vault
kubectl --context "$vault_context" -n "$vault_namespace" annotate secret \
  nico-vault-local-keys "nico.local/vault-pvc-uid=$vault_pvc_uid"
kubectl --context "$vault_context" -n "$vault_namespace" patch secret \
  nico-vault-local-keys --type=merge -p '{"immutable":true}'
python3 dev/deployment/stack/vault-bootstrap.py --context "$vault_context" \
  --namespace "$vault_namespace" bootstrap
```

Do not overwrite an existing Secret or attach another Vault's keys. After
successful recovery, remove the local recovery file. A changed mount TTL does
not extend an existing CA's expiration; bootstrap deliberately does not replace
an existing CA.

Removing only the Vault release retains its PVC and bootstrap Secrets.
Reinstalling Vault into the same cluster and namespace with the persistent
option recovers that Vault storage; it does not restore a deleted Core or REST
database or its site registration. Whole-stack teardown follows
[Stop and remove](#stop-and-remove), including its fresh-cluster reset. Keep `--persistent-vault=true` in the arguments after `--` when
invoking `tilt down` for this profile. To preserve the working stack, stop only
the Tilt process instead of tearing down its resources.

Deleting the kind cluster destroys this local storage. This option does not
provide cross-cluster backup/restore or automatic unsealing. Do not discard Vault
state while retaining guest disks whose credentials it owns. Durable Vault preserves the stack's local credentials across restarts.

## Existing local clusters

An earlier draft of this Tilt setup deployed Core as one Helm release named
`nico`. Remove that release once before starting the component-based setup:

```bash
helm uninstall nico -n nico-system
```

Tilt retains the selected MAT or BMC mode in a `kube-system` ConfigMap before
Core starts. An older cluster can acquire that marker automatically when its
matching MAT/PXE Helm release exists and the opposite-mode release is absent.
An opposite-mode release, or Core without either release, rejects startup on an
unmarked cluster. If startup is rejected, use a separate new kind cluster while
preserving the older cluster for inspection or recovery. An unmarked cluster
without Core or either release is treated as new.
Removing a marker or release does not convert retained site data between modes.

## Stop and remove

For BMC-backed labs, follow the [BMC lifecycle instructions](../lab/README.md#stop-restart-and-discard-the-bmc-lab).
The resource removal command below applies to the default MAT workflow.

Stopping `tilt up` leaves the stack running. Remove Tilt-managed resources with:

```bash
tilt down -f dev/deployment/tilt/Tiltfile
```

Pass the same profile flags the session ran with, or `tilt down` evaluates the
Tiltfile without those profiles and leaves their resources behind. After a DPF
session, for example, add `-- --dpf=true` so the DPF CRDs and the simulator are
removed too:

```bash
tilt down -f dev/deployment/tilt/Tiltfile -- --dpf=true
```

`tilt down` keeps namespaces, including `dpf-operator-system`, unless it also
gets `--delete-namespaces`.

Treat `tilt down` as teardown, not suspension: it removes Helm releases and can
remove database storage. Manually created resources such as the REST
`site-registration` Secret can survive. A retained registration belongs to its
original site; registration deliberately refuses to overwrite a different site
identity after a database reset.

To preserve a working stack, stop only the Tilt process and restart it with the
same options. After teardown, deploy into a fresh dedicated kind cluster. Once
all data in the disposable default cluster can be discarded:

```bash
kind delete cluster --name nico-tilt
bash dev/deployment/setup-kind.sh --name nico-tilt
export KUBECONFIG="$HOME/.kube/kind-nico-tilt.yaml"
```

Use the chosen cluster name in all three commands when it differs from
`nico-tilt`. The cluster deletion removes every workload and persistent volume
in that cluster; a new cluster avoids retaining credentials for a deleted site.

## Optional native guest lab

BMC-backed guests need a Linux runtime that exposes native KVM. Before preparing
that runtime, use the shared [lab preflight](../lab/README.md) to verify KVM
access and an explicit host budget. Then use the standalone
[native BMC target workflow](../../../helm/nico-bmc-target/README.md)
to build and exercise one guest through its dedicated Tilt entrypoint.
The default machine-a-tron workflow does not require this check.

## Native BMC-backed Scout lab

The opt-in `--bmc-lab` profile connects Core, DHCP and PXE to real Scout running
in native KVM guests. It requires persistent Vault and a supplied boot-artifact
image on a fresh kind cluster. Follow the
[Scout admission workflow](../lab/README.md#connect-scout-to-the-tilt-stack)
for image builds, target registration, BMC-specific stop/restart and full-discard
instructions, retained-state boundaries and the live
Scout check. Disk-installed instances use the
[native imager](../lab/README.md#native-disk-imager).
Ordinary Tilt development continues to use MAT by default.

For the optional BMC lab, `--dhcpv6` enables the existing DHCP chart's IPv6 service
on a single-node dual-stack cluster. It requires `--bmc-lab` and its persistent
Vault/boot-artifact inputs. The flag enables `nico-dhcp.dhcp.v6Enabled` and uses
server identifier `020000000006` only when `v6ServerIdentifier` is omitted. It
preserves other `nico-dhcp.dhcp` settings in `values.yaml`, including the
comma-separated IPv6 resolver list `v6DnsServers` and `v6LeaseTimers`. An omitted
resolver list supplies no DNS server. Configure a resolver reachable from the
guest networks before provisioning instances that need DNS, especially
IPv6-only guests. These values use the same contract as the
[DHCP chart](../../../helm/charts/nico-dhcp/values.yaml).

Configure the target relay and managed RA through shared chart values; see [managed DHCPv6 guests](../lab/README.md#managed-dhcpv6-guests).
The ordinary MAT workflow is unchanged.

### IPv6 provisioning endpoints

Add `--ipv6-endpoints` alongside `--dhcpv6` to expose IPv6 companion Services
for API port 1079 and PXE HTTP port 80 and to deploy the lab resolver. The
[IPv6 boot profile](#ipv6-boot-profile) changes the PXE port to 8080. The flag
is false by default and requires the same BMC lab and dual-stack kind cluster
as `--dhcpv6`. It leaves the internal Kubernetes Services in place.

The resolver's IPv6 ClusterIP is allocated by Kubernetes. Set
`nico-dhcp.dhcp.v6DnsServers` to that address in `values.yaml` before provisioning
guests, then explicitly trigger the manual `nico-dhcp` resource in Tilt. Set `network.dhcpv6.defaultRoute: true` on each
target, or provide a separate route from the guest to the IPv6 Service CIDR.
Configure the target's DHCPv6 relay separately. Endpoint exposure alone preserves
the default boot URLs and artifacts.

#### DHCPv6 lease-handoff build dependency

Repeated firmware, Scout, imager and installed-OS transitions require the
[DHCPv6 lease-handoff fixes in PR #7934](https://github.com/dsx-ai-factory/infra-controller/pull/7934)
until those changes are present on the source branch being built. Record the
resolved integration revision when testing an unmerged combination. The lab
helpers do not fetch or merge that dependency.

The native build image from that dependency retains its patched package at
`/opt/kea/kea-common.deb`. The shared development DHCP image installs that exact
package after checking its name, pinned Kea version and native architecture.
Both `make images-dev DEV_TARGET=dhcp` and Tilt use this packaging path. A build
image without the package retains the distribution Kea library and does not
establish the lease-handoff capability. An invalid supplied package fails the
build; it does not fall back to the distribution copy.

To verify the installed library bytes as well as both DHCP hook listeners, pass
the native package extracted from the selected build image to the existing smoke
check:

```bash
build_image=nico-dev-build:latest  # Use the actual DEV_BUILD_CONTAINER from the build.
container=$(docker create "$build_image" /not-started)
docker cp "$container:/opt/kea/kea-common.deb" /tmp/kea-common.deb
docker rm "$container"
bash dev/docker/tests/smoke_dhcp_image.sh nico-dev-dhcp:latest /tmp/kea-common.deb
```

The second argument is optional; without it, the smoke check tests tool presence,
hook loading and isolated UDP listeners as before. With it, the check also
requires the `kea-common` package name, installed version and native architecture
to match and compares every packaged `libkea*.so*` regular file with the runtime
copy. It copies the client-side package into a disposable container with networking
disabled, so an SSH Docker context does not need shared client paths. For Tilt,
use the exact resolved `build-container-localdev` image and deployed DHCP image
from that build. CI extracts the package from its selected build image and runs
this comparison when supplied; when the checkout contains the Kea package
producer (`dev/docker/build-kea.sh`), a missing package fails CI. A compiler
image from a lagging registry can therefore fail even if it starts normally:
rebuild or publish the matching compiler image before retrying. This prevents
a source checkout that requires the fix from silently testing stock libraries.
This packaging check does not replace the real instance lifecycle.

### IPv6 boot profile

For a fresh lab dedicated to IPv6 provisioning, also select `--ipv6-boot`.
The registered OS image URL must also be reachable over IPv6 by the imager.
An IPv6-accessible lab image server is sufficient; external IPv6 is only needed
when the image URL requires it.
It defaults to false and requires `--ipv6-endpoints`. Disable IPv4 provisioning
on **every** target (`network.provisioning.enabled: false`); this profile does
not support mixing IPv4 and IPv6 boot clients. Keep DHCPv6 relay and managed RA
enabled. Firmware can attempt DHCPv4 first, but must receive no DHCPv4 offer so
it reaches IPv6 HTTP boot. The native UEFI firmware and the lab's embedded iPXE
script support this path; other firmware is outside this lab's validation.

This profile changes the PXE companion to port 8080 and supplies client-facing
`https://carbide-api.forge:1079` and `http://carbide-pxe.forge:8080` URLs. Build
both Scout and imager with their `--ipv6-boot` option before packaging the
boot-artifact image. Copy their `*.network-profile` files alongside the EFI and
squashfs files; Tilt's init command rejects mismatched or missing IPv6 markers
before copying artifacts. Default builds retain their IPv4 resolver and URLs;
older unmarked artifacts are accepted only by the default boot profile.

With `--ipv6-boot`, the `ipv6-boot-endpoints` resource waits for `nico-pxe` and
`nico-lab-dns`, then exports their allocated IPv6 addresses into
`.local/CONTEXT/ipv6-endpoints.json`. DHCP depends on this export. The generated
Helm values supply `dhcp.v6ProvisioningServer` and `dhcp.v6DnsServers`; this profile
ignores explicit values for those two fields in `values.yaml`. Other DHCP
settings, including lease timers, retain their existing precedence. Trigger the
manual `nico-pxe` and `nico-dhcp` resources before powering on targets.
After replacing either Service during a Tilt session, trigger
`ipv6-boot-endpoints` successfully before triggering `nico-dhcp`; dependency
ordering does not automatically rerun the exporter after a Service replacement.

Direct Helm uses the same exporter after deploying the IPv6 PXE and DNS Services:

```bash
python3 dev/deployment/lab/endpoint-values.py \
  --context kind-nico-lab --output "$HOME/nico-ipv6-endpoints.json"
helm upgrade nico-dhcp helm/charts/nico-dhcp \
  --kube-context kind-nico-lab --namespace nico-system --reuse-values \
  --values "$HOME/nico-ipv6-endpoints.json" --wait --timeout 5m
```

This upgrade assumes the existing DHCP release already enables DHCPv6. Omit
`--set` overrides for the two endpoint fields: Helm gives explicit flags
precedence over values files. The exporter requires Python 3.9+ and kubectl.
`--context` and `--output` are required; `--namespace` defaults to `nico-system`.
It reads only the selected kind cluster's `nico-lab-dns` and `nico-pxe-external`
Services, verifies their Helm release ownership, single-stack IPv6 addresses and
DNS ports (UDP/TCP 53) or PXE port (TCP 8080), then writes JSON values. Parent
directories are created; a changed file is atomically replaced with mode 0600.
An unchanged file is left untouched. Lookup or validation failure returns nonzero
and preserves the previous file; do not deploy that stale file after a failure.
The helper neither changes cluster resources nor tests backend reachability.
Run it again after Service replacement and before reconciling DHCP.

BMC Service addresses and installed guest configuration are independent settings.
Use the [lab DNS chart](../../../helm/nico-lab-dns/README.md) for name-resolution details.

Keep both `--ipv6-endpoints` and `--ipv6-boot` enabled for the lifetime of the
dedicated IPv6 boot lab cluster. Changing
this profile in place is outside its support boundary: live Tilt argument
changes and stopping/restarting Tilt can handle removed resources differently.
Release the instances, then follow the
[stop/restart/discard procedure](../lab/README.md#stop-restart-and-discard-the-bmc-lab)
to stop both Tilt processes and discard the dedicated cluster. Create a fresh cluster for a
different topology; do not remove the resolver while guests depend on it.
