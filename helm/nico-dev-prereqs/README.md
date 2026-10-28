# Local development prerequisites

This chart defines local database credentials, CloudNativePG `Database` resources,
REST/Temporal and observability namespaces, and the Grafana dashboard ConfigMap.
Core trust and certificates remain in `helm-prereqs`; application workloads remain
in their component charts. Site registration and Vault recovery keep their own
writers and do not belong to this chart.

## Inputs

`coreNamespace` defaults to `nico-system`; `clusterName` defaults to
`nico-pg-cluster`. `localConfig` uses the `development.localConfig` mapping from
[shared deployment values](../../dev/deployment/stack/values.yaml). Its database
entries are `rest`, `keycloak`, and `temporal`, each with `resourceName`, `name`,
`user`, and `password`. Temporal also supplies `visibilityResourceName` and
`visibilityName`. The REST database requires `pg_trgm`.

`dashboards` maps filenames to JSON contents. Source paths are maintained in
`development.dashboardFiles`; the chart renders their contents without owning a
second copy of the dashboard sources. These are local fixture credentials, for isolated development environments only.

## Rendering and ownership

Tilt uses `helm template` and applies the rendered objects directly, preserving
its previous resource names, apply ownership, dependency graph, and delete behavior.
There is no additional Helm release, bootstrap hook, or automatic ownership
transfer. Do not install this chart as a release over objects already applied by
Tilt. A direct client can render and apply the same templates with the same values.

From the repository root, prepare chart values (Python requires PyYAML):

```bash
python3 - <<'PYTHON' > /tmp/nico-dev-prereqs.json
import json
from pathlib import Path
import yaml
profile = yaml.safe_load(Path('dev/deployment/stack/values.yaml').read_text())['development']
print(json.dumps({
    'localConfig': profile['localConfig'],
    'dashboards': {Path(p).name: Path(p).read_text() for p in profile['dashboardFiles']},
}))
PYTHON
helm template nico-dev-prereqs helm/nico-dev-prereqs \
  --values /tmp/nico-dev-prereqs.json --show-only templates/databases.yaml
```

Apply `rest-namespaces.yaml` before namespaced resources. Apply `credentials.yaml`
after the Core prerequisites and CNPG operator, before creating the PostgreSQL
cluster. Apply `databases.yaml` after that cluster exists, and wait for every
Database's `status.applied` to become true before starting consumers. The
observability namespace precedes `dashboards.yaml`. Rendering does not establish
readiness; the deploying client must perform those waits.

Deleting the rendered database objects and namespaces has the same consequences
as deleting their previously inline equivalents. This chart neither owns target
PVCs nor performs guest release, Vault recovery, or instance cleanup.
