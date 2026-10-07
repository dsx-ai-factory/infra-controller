# Drop-in Grafana dashboards

Any `*.json` Grafana dashboard placed in this directory is applied by
`install-observability.sh` (phase 8) as a ConfigMap labelled `grafana_dashboard=1` in the
`monitoring` namespace; the Grafana sidecar auto-loads it within ~a minute — no Grafana restart,
no manual import. Re-running the script updates changed dashboards in place.

The stack ships one default dashboard, **NICo Core Health** (`nico-core-health.json`): pod
status, restarts, scrape-target health, and API readiness for the `nico-system` namespace.
Workload-specific dashboards can be dropped in per site alongside it. The NICo umbrella chart can also
publish its own dashboards via `grafanaDashboards.enabled` (see `helm/values.yaml`); set
`grafanaDashboards.namespace: monitoring` so the sidecar (which watches the `monitoring`
namespace) picks them up.
