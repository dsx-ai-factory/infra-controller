# Local development with Tilt

Tilt runs the complete local NICo development stack from the repository's Helm
charts and Kubernetes sources. DevSpace remains available as a separate
workflow.

> [!WARNING]
> This workflow is intended only for local development and is
> provided as-is. Use it at your own risk.

## Requirements

- Docker connected to a Linux AMD64 or ARM64 daemon, with BuildKit available
- Tilt
- Helm 3 or Helm 4, plus Python 3 for Tilt's `helm_resource` extension
- `kubectl` connected to a native kind cluster; the setup helper below requires
  kind 0.32.0 or newer and `jq`

The Tiltfile refuses to load unless the active Kubernetes context starts with
`kind-`. This is stricter than Tilt's built-in local-cluster safety check and
prevents this development stack from targeting another cluster type.

## Create the development cluster

Run the shared cluster helper directly on macOS or Linux, with Docker, kind,
`kubectl` and `jq` on `PATH`. On macOS, start a local Docker runtime such as
Docker Desktop and select its Docker context. Kind and Tilt run from your macOS
terminal; a separately provisioned vfkit VM is not required. The runtime must
provide Linux AMD64 or ARM64 containers through a local Docker Unix socket.

The optional vfkit lab for BMC-controlled guests, bridged networking and an IPv6
gateway is separate work tracked in
[#8124](https://github.com/dsx-ai-factory/infra-controller/issues/8124). It is not a
prerequisite for this cluster setup. The Ubuntu host preparation script remains
responsible for Linux package installation and host configuration.

From the repository root:

```bash
bash dev/deployment/setup-kind.sh --name nico-tilt
export KUBECONFIG="$HOME/.kube/kind-nico-tilt.yaml"
kubectl --context kind-nico-tilt get nodes
```

The default `nico-tilt` name is separate from DevSpace's `nico-dev`. Select an
unused cluster for Tilt: this helper does not detect or migrate an existing
DevSpace deployment, and both frontends deploy into the same NICo namespaces.
For deliberate reuse after removing a DevSpace deployment, see
[existing local clusters](#existing-local-clusters).

For an isolated dual-stack cluster, select a different name:

```bash
bash dev/deployment/setup-kind.sh --name nico-dual --ip-family dual
export KUBECONFIG="$HOME/.kube/kind-nico-dual.yaml"
kubectl --context kind-nico-dual get nodes -o wide
```

Both architectures use the same helper and configuration. Dual stack requires
IPv6 support in the container runtime's Linux kernel and Docker network, but not
Internet IPv6.
It configures Kubernetes pod/service networking; it does not configure NICo
underlays, tenant prefixes, DHCPv6, SLAAC or instance networking.

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
architecture. On Linux it must match the host; on macOS it comes from the local
container runtime, independently of the client process architecture. The helper
does not detect whether a macOS runtime is using hardware emulation. SSH/TCP
Docker endpoints are rejected because this helper does not arrange remote API
access; run it on that Docker host instead.
A `DOCKER_HOST` Unix socket is honored with Docker's default context.

Validation uses a private temporary kubeconfig with explicit context selection;
inherited `KUBECONFIG` is ignored. Your normal kubeconfig and current context
remain unchanged unless you explicitly select that file with `--kubeconfig`.
New clusters publish their API on `127.0.0.1`, on a port chosen by kind. Run Tilt
from the same macOS or Linux client, using the exported kubeconfig and the same
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

When adding a second cluster, kubelet/containerd errors about "too many open
files" can indicate exhausted inotify resources in the runtime's Linux kernel.
On macOS these limits belong to the container runtime, not macOS sysctl. Follow
[kind's inotify troubleshooting](https://kind.sigs.k8s.io/docs/user/known-issues/#pod-errors-due-to-too-many-open-files)
to diagnose and size those Linux limits through the runtime; the helper does not
change sysctls.
Image imports with Docker's containerd image store can require
[kind's native-platform archive workaround](https://kind.sigs.k8s.io/docs/user/known-issues/#unable-to-kind-load-docker-images).

This step installs no NICo services, host packages, registry workarounds or BMC
targets, and does not modify the Docker daemon configuration. The DevSpace host
bootstrap calls the same helper, then applies its existing node TLS settings and
continues its own deployment workflow.

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
- Temporal and its local namespaces
- Keycloak and the `nico-dev` realm
- NICo REST API, database migrations, certificate manager, site manager,
  workflow workers, and site agent
- NICo MCP
- automatic local site registration

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

Tilt passes `tilt.observability.postgresExporter` as chart values, supplying the
image and `tilt.localConfig.database` host, port, and database name. Development
values use `database.credentialsSecret: nico-pg-cluster-app` and
`database.sslMode: disable`; the credentials Secret is in the release namespace.
For verified TLS, configure `database.sslMode` and `database.sslRootCertSecret`
under `tilt.observability.postgresExporter`. Tilt enables the ServiceMonitor and
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
Performance** dashboards into the **NICo** Grafana folder. The Tilt-local Site
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

All Tilt settings are in [`values.yaml`](values.yaml). NICo Core values are at
the document root, while the `tilt` section contains the prerequisite and REST
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

All Tilt runtime images derive from one digest-pinned Debian Bookworm slim base
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

The `tilt.profiles` switches in [`values.yaml`](values.yaml) control REST, MCP,
DSX Exchange, and observability. REST brings Temporal and Keycloak, MCP
automatically brings REST, and DSX Exchange brings NATS. Observability manages
Loki, Alloy, Prometheus, Grafana, and the NICo metrics monitors as one unit. All
optional profiles are disabled by default.

Changes to `values.yaml` reload the running Tilt session. For temporary
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

Return to the values-file defaults with:

```bash
tilt args --clear
```

## Existing local clusters

An earlier draft of this Tilt setup deployed Core as one Helm release named
`nico`. Remove that release once before starting the component-based setup:

```bash
helm uninstall nico -n nico-system
```

Tilt and DevSpace deploy the same application object names and should not manage
the same cluster simultaneously. Before switching an existing DevSpace kind
cluster to Tilt, run:

```bash
LOCAL_DEV_INSTALL_REST_PREREQS=0 devspace purge -n nico-system
```

To switch back to DevSpace, remove the Tilt stack and use the existing DevSpace
bootstrap and deploy commands:

```bash
tilt down -f dev/deployment/tilt/Tiltfile
dev/deployment/devspace/bootstrap-prereqs.sh
devspace deploy -n nico-system
```

## Stop and remove

Stopping `tilt up` leaves the stack running. Remove Tilt-managed resources with:

```bash
tilt down -f dev/deployment/tilt/Tiltfile
```
