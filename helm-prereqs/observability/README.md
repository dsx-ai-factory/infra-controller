# Observability — site-local metrics, logs, and traces for a NICo cluster

An optional, self-contained monitoring stack for any cluster installed with `helm-prereqs`:
**Prometheus** (metrics) + **Grafana** (UI) + **Loki** (logs) + **Tempo** (traces) + an
**OpenTelemetry collector** that feeds them. Fully self-contained: everything runs and stays
**on the site**; nothing leaves the cluster.

```text
                     ┌───────────────────────────── monitoring ns ─────────────────────────────┐
 NICo /metrics ──────► Prometheus (kube-prometheus-stack, release `obs`) ◄── ServiceMonitors    │
 (carbide_*)         │        │                                                                 │
                     │        ▼                                                                 │
                     │     Grafana ── datasources: Prometheus, Loki, Tempo; dashboard sidecar   │
                     └────────▲───────────────────────▲─────────────────────────────────────────┘
                              │                       │
 pod stdout ──► otel-agent (DaemonSet, otel ns) ──► Loki (loki ns, :3100)
 OTLP spans ──► otel-agent :4317/:4318 ─────────► Tempo (tempo ns, :4317)
 DPU logs/metrics ──► otel-collector-gateway (optional, mTLS VIP :443) ──► Loki + Prometheus
```

## What gets collected

| Signal | Source | Store | How |
|---|---|---|---|
| Metrics | every NICo service's `/metrics` (`carbide_*`) | Prometheus | the NICo charts' ServiceMonitors — auto-enabled in the setup.sh flow, see [Scraping the NICo metrics](#scraping-the-nico-metrics) |
| Metrics | nodes, cadvisor, kube-state | Prometheus | kube-prometheus-stack defaults |
| Logs | ALL pod stdout (nico-\*, nico-rest, flow, temporal, vault, CP static pods, …) | Loki | otel-agent DaemonSet tails `/var/log/pods/*` |
| Traces | NICo OTLP spans (opt-in, see [Enabling traces](#enabling-traces)) | Tempo | services → otel-agent `:4317` → Tempo |
| DPU logs + host metrics | BlueField `nico-otelcol` (real-DPU sites only) | Loki + Prometheus | OTLP/mTLS gateway, `WITH_DPU=true` |

Every log line and trace is tagged `forge_site=<site>`; Prometheus carries it as an external
label (attached to anything it exports, e.g. alerts).

## Components and pins

| Release | Namespace | Chart | Version | Notes |
|---|---|---|---|---|
| `obs` | monitoring | prometheus-community/kube-prometheus-stack | 59.1.0 | release name is load-bearing (rendered names `obs-grafana`/`obs-prometheus` are referenced by the script, clean.sh, and these docs) |
| `loki` | loki | grafana/loki | 5.15.0 (Loki 2.8.4) | single-binary; Service name `loki` is load-bearing |
| `tempo` | tempo | grafana-community/tempo | 2.2.3 (Tempo 2.10.x) | monolithic; the chart moved off `grafana/helm-charts`; Service name `tempo` is load-bearing; query API :3200, OTLP ingest :4317/:4318 |
| `otel-agent` | otel | open-telemetry/opentelemetry-collector | 0.106.0 (image contrib:0.106.1) | **image pin is load-bearing** — the `loki` exporter was deprecated upstream in v0.107 and removed from the contrib distribution soon after; bumping requires moving logs to Loki-native OTLP + Loki 3.x |
| `otel-collector-gateway` | otel | open-telemetry/opentelemetry-collector | 0.106.0 | optional (`WITH_DPU=true`) |

## Quick start

**With a fresh install** (any mode — works with `--skip-rest` / infra-only too):

```bash
./setup.sh --with-observability [your usual flags...]
# equivalently: WITH_OBSERVABILITY=true ./setup.sh [your usual flags...]
```

**Standalone, on an existing cluster** (installed last month or last year — no setup.sh state
needed beyond the `local-path-persistent` StorageClass):

```bash
helm-prereqs/observability/install-observability.sh
```

Both are idempotent (`helm upgrade --install` + server-side apply); re-run freely to reconcile.
Then open Grafana:

```bash
kubectl -n monitoring port-forward svc/obs-grafana 3000:80   # -> http://localhost:3000
```

## Configuration (env vars)

| Variable | Default | Effect |
|---|---|---|
| `OTEL_SITE_NAME` | `siteName` from `helm-prereqs/values.yaml`, else `nico-site` | the `forge_site` label on every signal |
| `GRAFANA_VIP` | *(unset — ClusterIP + port-forward)* | expose Grafana on a MetalLB VIP (must exist in the pool; add DNS if you want a name) |
| `GRAFANA_ANONYMOUS` | `true` | `false` = require login (see [Securing Grafana](#securing-grafana)) |
| `PROMETHEUS_OPERATOR` | `true` | set `false` if the cluster already runs prometheus-operator |
| `WITH_TEMPO` | `true` | `false` = skip Tempo and the agent's traces pipeline (metrics + logs only) |
| `WITH_DPU` | `false` | also install the DPU OTLP/mTLS gateway (real BlueFields only) |
| `OTEL_RECEIVER_VIP` | *(required if `WITH_DPU`)* | MetalLB VIP the DPUs dial |
| `OTEL_RECEIVER_DNS` | `otel-receiver.forge` | SAN on the gateway cert = the name DPUs dial |
| `SITE_CA_ISSUER` | `site-issuer` | ClusterIssuer for the gateway cert — **must be the CA the DPUs trust** |
| `NICO_SERVICEMONITORS` | `hint` (standalone), `true` (from setup.sh) | `true` = enable the NICo charts' ServiceMonitors via a release upgrade with this tree's chart; `hint` = print the enable command instead; `false` = skip (see [Scraping the NICo metrics](#scraping-the-nico-metrics)) |
| `NICO_RELEASE` / `NICO_NS` | `nico` / `nico-system` | NICo Core release name / namespace |
| `LOKI_CHART_VER` `TEMPO_CHART_VER` `OTEL_CHART_VER` `KPS_CHART_VER` | see [pins](#components-and-pins) | chart version overrides |

Per-site values are applied as `helm --set-string` overrides — the `values-*.yaml` files keep
validated defaults and never need editing for a new site.

## Scraping the NICo metrics

The NICo subcharts ship ServiceMonitor templates but **default them off**, and the standard
deploy values do not enable them — a fresh site exposes `carbide_*` metrics that nothing
scrapes. In the `setup.sh --with-observability` flow **the installer closes this
automatically**: Core was just installed from the same tree, so it safely applies the bundled
`values-nico-servicemonitors.yaml` overlay to the release (`--reuse-values`; adds monitor
objects only, no pod changes). Prometheus discovers them cluster-wide within a scrape interval
(`*SelectorNilUsesHelmValues: false` — no labels to coordinate).

Running **standalone** the default is `hint`: upgrading a release with a chart tree that may
differ from what is deployed could apply more than the monitors, so the installer prints the
enable command instead. Apply it with the chart ref your site was actually installed from:

```bash
helm upgrade nico <your-chart-ref> -n nico-system --reuse-values \
    -f helm-prereqs/observability/values-nico-servicemonitors.yaml
```

(or re-run with `NICO_SERVICEMONITORS=true` when this checkout matches the deployed chart).
If Core is not installed at all (infra-only cluster), the step defers with the same hint.

## Enabling traces

NICo's span export is **off by default** and ships in the binaries already — enabling it is
config-only. In the nico-api site config (`siteConfig.nicoApiSiteConfig` in your Core values):

```toml
[tracing]
enabled = true
otlp_endpoint = "http://otel-agent.otel.svc.cluster.local:4317"
```

or set `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT=http://otel-agent.otel.svc.cluster.local:4317` via
the chart's generic `extraEnv:` (the env var wins over the config key — but it sets ONLY the
endpoint; `enabled` must come from the TOML, or at runtime without a pod roll:
`nico-admin-cli set tracing-enabled true`). Details, verified from `crates/api/src/logging.rs`
+ `crates/api-core/src/cfg/file.rs` (see also `docs/observability/tracing.md`):

- transport is OTLP **gRPC, plaintext only** — never point nico-api at `:4318` or an `https://`
  endpoint; keep the hop in-cluster (the collector is the TLS boundary);
- sampling is NOT sample-everything: `CarbideSpanSampler` records only spans marked
  `carbide.trace_root = true` (today: every inbound REST/gRPC request, the state-controller
  reconcile loops, and site-explorer runs — children inherit). Sparse traces are by design;
- **set the endpoint permanently even if you leave the flag off**: with no endpoint configured
  nico-api doesn't forward inbound trace context either, so traces break at that hop. With an
  endpoint and the flag off, overhead is near zero. Tracing IS expensive when on — treat it as
  a debug-session tool (toggle on, investigate, toggle off); for sustained use add a
  tail-sampling policy at the collector (`docs/observability/tracing.md` §2.1 has one);
- `nico-dns` can also emit spans, but is off by default (no hardcoded endpoint): set
  `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` (the `nico-dns` chart's `otlpEndpoint` value sets this),
  or `--otlp-endpoint`/`otlp_endpoint` in its config file, to enable it. Earlier versions
  exported unconditionally to a hardcoded
  `opentelemetry-collector.otel.svc.cluster.local:4317` default; the installer's ExternalName
  alias with that name still resolves for deployments relying on that address, but new
  deployments should point `otlpEndpoint` at the agent explicitly. The other services (pxe,
  dhcp, …) build no exporter today;
- spans arrive in Tempo tagged `service.name=carbide-api` (`service.name=nico-dns` for `nico-dns`
  spans); the Grafana Tempo datasource is
  pre-provisioned with trace-to-logs linking into Loki (one direction only: NICo log lines
  carry `span_id` but no `trace_id`, so Loki→Tempo derived fields aren't wired).

## Dashboards

One default dashboard ships with the stack: **NICo Core Health** (nico-system pod status,
restarts, scrape-target health, API readiness). Drop any additional Grafana dashboard JSON
into `observability/dashboards/` and (re-)run the install script — each becomes a
`grafana_dashboard`-labelled ConfigMap that the Grafana sidecar auto-loads (see
`dashboards/README.md`). The NICo umbrella chart can additionally publish its
own dashboards via `grafanaDashboards.enabled` in `helm/values.yaml` — set
`grafanaDashboards.namespace: monitoring` too, since this Grafana's sidecar only watches the
`monitoring` namespace.

## Securing Grafana

The default is **no auth** — anonymous users are Admin and the login form is disabled. That
matches the rest of the stack (Loki and the collectors are auth-free) and is intended for
internal-only site clusters reached over a VIP or tunnel. If your Grafana is reachable more
widely:

```bash
GRAFANA_ANONYMOUS=false ./install-observability.sh
# login: admin / $(kubectl -n monitoring get secret obs-grafana -o jsonpath='{.data.admin-password}' | base64 -d)
```

## Verify

```bash
# pods up
kubectl -n loki get pods && kubectl -n tempo get pods && kubectl -n otel get pods && kubectl -n monitoring get pods

# logs flowing (Grafana -> Explore -> Loki), or raw:
kubectl -n loki exec sts/loki -- wget -qO- \
  'http://localhost:3100/loki/api/v1/query?query=count_over_time({forge_site="<site>"}[5m])' | head -c 400

# NICo metrics scraped (after enabling ServiceMonitors): Grafana -> Explore -> Prometheus ->
#   up{job=~"nico-.*"}     and     carbide_api_ready

# traces (after enabling span export): Grafana -> Explore -> Tempo -> search service carbide-api
```

## Troubleshooting / gotchas

- **Loki retention silently not deleting** — the compactor needs `working_directory` +
  `shared_store` on Loki 2.8.4 (already set in `values-loki.yaml`); without them the 50Gi PVC
  grows unbounded. On a Loki 3.x bump `shared_store` becomes `delete_request_store`.
- **kube-prometheus-stack CRDs** — the chart runs `crds.enabled=false`; the install script
  server-side-applies only the *missing* `monitoring.coreos.com` CRDs, never touching existing
  ones (a helm-prereqs cluster already owns them via `operators/crds/`).
- **`kubeEtcd` target DOWN** — kubespray binds etcd metrics to `127.0.0.1:2381`, so etcd
  scraping is disabled in the values; re-enable on clusters with a routable etcd listener.
- **Grafana LoadBalancer `<pending>`** — the VIP must already exist in the MetalLB pool;
  apply the pool change first, then re-run the install.
- **DPU gateway x509 "unknown authority" on the DPU** — the gateway cert was issued by a CA
  the DPU doesn't trust. `SITE_CA_ISSUER` must be the ca-issuer backed by the site root the
  DPUs hold (their `/etc/otelcol-contrib/certs/ca.pem`); a dev-CA issuer will not work. If
  re-pointing the issuer and cert-manager won't reissue: `kubectl -n otel delete secret
  otel-receiver-tls` and re-run.
- **Gateway VIP** — dedicated VIP, `externalTrafficPolicy: Local` (the DPU dials on-subnet);
  never share a VIP between Services with different ETP (MetalLB wedges).
- **journald logs** (systemd-only services on nodes) — not collected; the contrib image ships
  no journalctl. Adding it needs a journald-capable image (`values-otel-collector-agent.yaml`
  header has the notes).
- **`GRAFANA_VIP`/`OTEL_RECEIVER_VIP` on a cluster without MetalLB** — the LoadBalancer Service
  never gets an address and helm `--wait` blocks until its timeout, failing the install. Leave
  the VIP vars unset (ClusterIP + port-forward) unless MetalLB (or another LB controller) is
  running with the address in a pool.
- **Pod Security Admission `restricted` namespaces** — the otel-agent runs as root with a
  hostPath mount (to read `/var/log/pods`) and node-exporter needs hostNetwork; both are
  rejected under an enforced `restricted` policy. Label the `otel` and `monitoring` namespaces
  `privileged` (or relax enforcement) before installing on such clusters.

## Uninstall

`helm-prereqs/clean.sh` removes the stack with everything else, or by hand:

```bash
helm uninstall obs -n monitoring; helm uninstall otel-agent otel-collector-gateway -n otel
helm uninstall tempo -n tempo;    helm uninstall loki -n loki
kubectl delete ns monitoring otel tempo loki
```

The PersistentVolumes (Loki/Tempo/Prometheus data) go `Released` and keep their on-node data —
the `local-path-persistent` StorageClass retains. `clean.sh` deletes those Released PVs; after
a manual uninstall, remove them yourself (`kubectl get pv | grep Released`). On-node data
directories are only reclaimed when the PV is deleted through the provisioner.

## Files

| File | Role |
|---|---|
| `install-observability.sh` | the installer (setup.sh phase AND standalone) |
| `values-loki.yaml` | Loki single-binary + retention/compactor config |
| `values-tempo.yaml` | Tempo monolithic + OTLP ingest |
| `values-otel-collector-agent.yaml` | DaemonSet: pod logs → Loki, OTLP spans → Tempo |
| `values-otel-collector-gateway.yaml` | optional DPU OTLP/mTLS gateway |
| `values-kube-prometheus-stack.yaml` | Prometheus + Grafana + datasources + sidecar |
| `values-nico-servicemonitors.yaml` | Core overlay: turn on the NICo ServiceMonitors |
| `otel-receiver-certificate.yaml` | gateway server cert (WITH_DPU; issuer/SAN rewritten by the script) |
| `dashboards/` | drop-in Grafana dashboards (auto-loaded); ships `nico-core-health.json` |

## Not included

Alerting (alertmanager ships disabled — enable it in `values-kube-prometheus-stack.yaml` when
wanted), journald collection (see the agent values header), and multi-tenant auth (Loki and
the collectors are auth-free; the stack is designed for internal-only site clusters).
