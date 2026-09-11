# Local development with Tilt

Tilt runs the complete local NICo development stack from the repository's Helm
charts and Kubernetes sources. DevSpace remains available as a separate
workflow.

> [!WARNING]
> This workflow is intended only for local development and is
> provided as-is. Use it at your own risk.

## Requirements

- Docker
- Tilt
- Helm 3 or Helm 4, plus Python 3 for Tilt's `helm_resource` extension
- `kubectl` connected to a kind cluster

The Tiltfile refuses to load unless the active Kubernetes context starts with
`kind-`. This is stricter than Tilt's built-in local-cluster safety check and
prevents this development stack from targeting another cluster type.

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

All Tilt settings are in [`values.yaml`](values.yaml). NICo Core values are at
the document root, while the `tilt` section contains the prerequisite and REST
settings.

## Machine-a-tron BMC routing

Tilt runs machine-a-tron in controller mode. `mat-k8s-controller` polls the
machine-a-tron status endpoint and creates one Kubernetes ClusterIP Service per
simulated BMC, using the BMC address assigned by NICo as the Service IP. NICo
therefore connects directly to each simulated BMC instead of routing every
Redfish request through the shared machine-a-tron proxy.

The Tilt BMC underlay is `10.96.64.0/18`, which lies within Kind's default
`10.96.0.0/16` ServiceCIDR. Kubernetes rejects an explicit ClusterIP outside
the cluster ServiceCIDR, so keep this range aligned with the Kind network
configuration if the ServiceCIDR changes.

## Image builds

Core uses the existing Dockerfiles under
[`dev/deployment/devspace/`](../devspace); mat-k8s-controller uses its existing
[`Dockerfile`](../../k8s/machine-a-tron-controller/Dockerfile). REST and MCP
use the existing Dockerfiles under
[`rest-api/docker/local/`](../../../rest-api/docker/local).
The Tilt setup does not add or modify a Dockerfile.

Each deployable image has its own Tilt resource and rebuild control. Images build
once during startup, then wait for their resource's update button before
rebuilding. REST build contexts are restricted to the source directories copied
by that image's Dockerfile. Docker and Cargo or Go build caches are reused across
rebuilds.

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
