# NICo PostgreSQL exporter

This optional chart installs the NICo-owned PostgreSQL metrics exporter and the
**NICo / PostgreSQL** Grafana dashboard. It connects directly to PostgreSQL and
has no dependency on CloudNativePG, Tilt, or a bundled Grafana installation.
It is disabled by default, both standalone and in the NICo umbrella chart.
Operators supply PostgreSQL, Prometheus, and Grafana.

## Install

In umbrella-chart site values, enable and configure the subchart:

```yaml
nico-postgres-exporter:
  enabled: true
  image:
    repository: registry.example.com/nico-postgres-exporter
    tag: your-version
  database:
    host: postgres.example.com
    name: nico
    credentialsSecret: postgres-exporter-credentials
    sslMode: verify-full
    sslRootCertSecret:
      name: postgres-ca
      key: ca.crt
  serviceMonitor:
    enabled: true
    labels:
      release: prometheus
  grafanaDashboards:
    namespace: monitoring
```

The image must contain `/usr/local/bin/nico-postgres-exporter` and work as UID
65532 with a read-only filesystem. The chart requires its own image repository
and tag; it does not inherit the Core image. Tilt builds and supplies this image.
Create the credential and optional CA Secrets in the Helm release namespace.
Credentials default to Secret keys `username` and `password`; override them with
`database.usernameKey` and `database.passwordKey`. Secrets are referenced, never
created by this chart. The dashboard contains no database credentials.

For a separate release, put the same settings in a values file **without** the
outer `nico-postgres-exporter:` key and install:

```bash
helm upgrade --install nico-postgres-exporter ./helm/charts/nico-postgres-exporter \
  --namespace nico-system --create-namespace \
  --values postgres-exporter-values.yaml
```

## Chart settings

These are chart defaults; site values can override them. See
[values.yaml](values.yaml) for the complete settings, including resource limits.

| Setting | Default | Contract |
| --- | --- | --- |
| `enabled` | `false` | Gates every resource; also controls the umbrella dependency. |
| `exporter.enabled` | `true` | Deployment and Service; required for a ServiceMonitor. Set false for dashboard-only installs. |
| `image.repository`, `image.tag` | Empty | Both required when the exporter is enabled. |
| `image.pullPolicy`, `imagePullSecrets` | `IfNotPresent`, `[]` | Kubernetes pull policy and release-namespace image pull Secret references. |
| `fullnameOverride` | Empty | Overrides resource name prefix; otherwise derived from release and chart names. |
| `database.host`, `database.name`, `database.credentialsSecret` | Empty | All required when the exporter is enabled. Credentials Secret must be in the release namespace. |
| `database.port` | `5432` | PostgreSQL port, accepted by the binary from 1 through 65535. |
| `database.usernameKey`, `database.passwordKey` | `username`, `password` | Keys in the existing credentials Secret. |
| `database.sslMode` | `require` | TLS mode; see binary configuration below. Use `verify-full` to verify CA and hostname. |
| `database.sslRootCertSecret.name`, `.key` | Empty, `ca.crt` | Optional existing CA Secret and key; mounted as `/var/run/secrets/postgres-ca/ca.crt`. Empty name omits the mount. |
| `serviceMonitor.enabled` | `false` | Requires Prometheus Operator CRDs when enabled. |
| `serviceMonitor.labels` | `{}` | Additional monitor metadata labels for the site's Prometheus selector. |
| `serviceMonitor.interval`, `.scrapeTimeout` | `30s`, `10s` | Prometheus scrape durations; timeout must not exceed the interval. |
| `grafanaDashboards.enabled` | `true` | A separate ConfigMap containing the dashboard JSON when the chart is enabled. Independent of the exporter. |
| `grafanaDashboards.namespace` | Empty | Defaults to the release namespace. An override namespace must exist and Helm must be allowed to write there. |
| `grafanaDashboards.labels` | `grafana_dashboard: "1"` | Dashboard-discovery labels; match your Grafana sidecar's selector. Set the default label to null to remove it. |
| `grafanaDashboards.folder`, `.folderAnnotation` | `PostgreSQL`, `grafana_folder` | Folder annotation for compatible Grafana sidecars. Empty either setting to omit it. |
| `grafanaDashboards.annotations` | `{}` | Additional annotations. The configured folder overrides an annotation with the same key. |

The exporter exposes `/metrics` on Service port 9090. With a ServiceMonitor,
configure your Prometheus instance to select its labels and watch its namespace.
The monitor copies `component=nico-postgres-exporter` from the Service into scrape
target labels. If you scrape without Prometheus Operator, configure your own
scrape job for this Service and preserve that label: the dashboard filters on it.
Use one scrape mechanism per exporter.

## Use your own Grafana

Grafana does not discover arbitrary Helm charts. This chart publishes a ConfigMap
that a configured dashboard sidecar can discover. By default it has label
`grafana_dashboard: "1"` and annotation `grafana_folder: PostgreSQL`. Configure
its namespace, labels, and folder annotation to match your Grafana sidecar. The
sidecar needs permission to watch that namespace and folder provisioning enabled.
The PostgreSQL dashboard stays separate from the umbrella's Core dashboard
ConfigMap, which uses the NICo folder.

Without a sidecar, import
[dashboards/nico-postgres.json](dashboards/nico-postgres.json) through Grafana's
**Dashboards → New → Import**, or provision that file using your existing Grafana
file/API workflow. Select your Prometheus datasource in the dashboard. The stable
UID is `nico-postgres`; importing the same UID updates that dashboard. Helm
upgrades update the sidecar ConfigMap; manual imports require reimporting the JSON.

To install only the discoverable dashboard ConfigMap, no image, credentials, or
Prometheus Operator CRDs are required:

```yaml
enabled: true
exporter:
  enabled: false
grafanaDashboards:
  namespace: monitoring
```

Set `grafanaDashboards.enabled: false` to install the exporter without publishing
a ConfigMap, for example when importing the JSON manually. A dashboard-only
install still needs a separately deployed and scraped exporter to show data.

## Database collection and dashboard

The dashboard shows exporter scrape health,
the latest database probe result, the last successful probe time, query-statistics
collection health, and a **Slow queries** table. The collector executes read-only
`SELECT 1` and reads the top 20 `pg_stat_statements` entries by total execution
time immediately and every 30 seconds. Each collection has a five-second deadline
including connection acquisition, and both use one pooled connection.

The table shows query text, calls, total execution time in seconds, and mean
execution time in milliseconds, sorted by total time descending. These are
cumulative statistics since the entry was created or PostgreSQL statistics were
reset, independent of the dashboard time range. Entries from different databases,
roles, or top-level/nested execution remain separate even if their SQL text matches.
Only the latest top 20 entries per exporter are exposed; a failed collection clears
the snapshot and sets query-statistics health to zero. An empty successful snapshot
also has no table rows but keeps query-statistics health at one.

For an external database, preload `pg_stat_statements`, install its extension in
the monitored database, and grant `pg_read_all_stats` to the exporter login to see
other roles' query text and IDs. Without that membership, PostgreSQL can redact
those fields, causing statement collection to fail. The connectivity probe only
requires CONNECT permission and still works when statement collection fails.

| Metric | Type | Meaning |
| --- | --- | --- |
| `nico_postgres_exporter_database_up` | Gauge | Latest query succeeded: `1`; initial, failed, or timed-out probe: `0`. |
| `nico_postgres_exporter_last_success_timestamp_seconds` | Gauge | Unix seconds of the latest successful query; initially `0`, preserved on failure. |
| `nico_postgres_exporter_stat_statements_up` | Gauge | Latest statement collection succeeded: `1`; initial, failed, or timed-out collection: `0`. |
| `nico_postgres_statement_calls` | Gauge | Cumulative calls for each entry in the latest top 20 snapshot. |
| `nico_postgres_statement_exec_seconds` | Gauge | Cumulative execution seconds for each entry in that snapshot. |
| `nico_postgres_statement_mean_exec_milliseconds` | Gauge | Mean execution milliseconds for each entry in that snapshot. |

The three statement gauges carry `query` (PostgreSQL's representative SQL text)
and `statement_id` (`dbid:userid:queryid:toplevel`) labels. Query text is retained
in Prometheus and may include literals in utility statements. The health and
timestamp gauges have no exporter-defined labels. The
ServiceMonitor adds `component=nico-postgres-exporter`, alongside Prometheus's
normal target labels. Prometheus `up` measures exporter scrape health separately
from database connectivity. Missing series show **No data**; the last-success
panel shows **Never** for zero. A database outage keeps `/metrics`, `/health`, and
`/ready` available, with database health at zero. If the exporter stops,
Prometheus scrape health goes down and the connection panel shows **No data**.

The chart does not change PostgreSQL configuration or grants. Connectivity can
be monitored without the statistics extension or grant.

## Binary configuration

The binary supports these connection settings, independently of Tilt and CNPG:

| Flag | Environment variable | Default / requirement |
| --- | --- | --- |
| `--host` | `PGHOST` | Required nonempty hostname or Unix socket directory. |
| `--port` | `PGPORT` | `5432`; accepted range `1`–`65535`. |
| `--database` | `PGDATABASE` | Required nonempty database name. |
| `--username` | `PGUSER` | Required nonempty login role. |
| `--password` | `PGPASSWORD` | Required; empty is accepted for passwordless authentication. |
| `--ssl-mode` | `PGSSLMODE` | `require`; accepts `disable`, `allow`, `prefer`, `require`, `verify-ca`, `verify-full`. |
| `--ssl-root-cert` | `PGSSLROOTCERT` | Optional readable PEM CA bundle; used for verified TLS. |
| `--listen` | `NICO_POSTGRES_EXPORTER_LISTEN` | `0.0.0.0:9090`; an IP address and port. |

TLS mode names are case-insensitive. Flags override the corresponding
environment variables. Configuration changes require restarting the process.
Use Secret-backed environment variables for
passwords. For external PostgreSQL with certificate verification, set
`PGSSLMODE=verify-full` and mount the CA bundle at `PGSSLROOTCERT`; the host must
match the server certificate. The chart mounts an existing CA Secret when
`database.sslRootCertSecret.name` is configured. Connection failures are retried on the
next interval; malformed configuration or an unreadable CA file fails startup.
`RUST_LOG` controls the structured log filter and defaults to `info`.
