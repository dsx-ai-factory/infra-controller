# NICo REST Production Quick Start

This guide deploys the NICo REST control plane running on an existing Kubernetes cluster. For a full explanation of each component and production configuration options, see [INSTALLATION.md](INSTALLATION.md).

**Prerequisites:**

- Kubernetes cluster (v1.27+) with cluster-admin access
- [cert-manager](https://cert-manager.io/docs/installation/) installed (v1.13+)
- `helm` v3, `kubectl`, `docker`, `make`

---

## 1. Build and Push Images

```bash
REGISTRY=my-registry.example.com/nico
TAG=v1.0.0

make docker-build IMAGE_REGISTRY=$REGISTRY IMAGE_TAG=$TAG

for image in nico-rest-api nico-rest-workflow nico-rest-site-manager \
             nico-rest-site-agent nico-rest-db nico-rest-cert-manager; do
    docker push "$REGISTRY/$image:$TAG"
done
```

Then update the `images:` stanza in each overlay under `deploy/kustomize/overlays/` with your registry and tag.

---

## 2. Create Namespaces

```bash
kubectl create namespace nico-rest
kubectl apply -f deploy/kustomize/base/postgres/namespace.yaml
kubectl apply -f deploy/kustomize/base/temporal-helm/namespace.yaml
```

---

## 3. Generate the CA Signing Secret

```bash
./scripts/gen-site-ca.sh
```

Creates `ca-signing-secret` in both `nico-rest` and `cert-manager` namespaces. This is the trust anchor for all TLS in the deployment — every certificate issued to NICo REST workloads traces back to it.

To bring your own CA instead, see [INSTALLATION.md — Step 2](INSTALLATION.md#step-2--create-the-ca-signing-secret).

---

## 4. Deploy PostgreSQL and Keycloak

> If you already have a PostgreSQL instance, skip the PostgreSQL apply and go straight to Step 7 (migrations). See [INSTALLATION.md — Step 3](INSTALLATION.md#step-3--deploy-postgresql) for the databases and users that must exist.

```bash
# PostgreSQL
kubectl apply -k deploy/kustomize/base/postgres
kubectl rollout status statefulset/postgres -n postgres

# Keycloak
kubectl apply -k deploy/kustomize/base/keycloak
```

---

## 5. Deploy the PKI Stack

```bash
# Internal PKI service
kubectl kustomize --load-restrictor LoadRestrictionsNone \
  deploy/kustomize/overlays/cert-manager | kubectl apply -f -

# ClusterIssuer for cert-manager.io
kubectl apply -k deploy/kustomize/base/cert-manager-io

# Shared secrets and Temporal client certificate
kubectl apply -k deploy/kustomize/base/common
```

---

## 6. Deploy Temporal

```bash
# Apply namespace, DB credentials, and TLS Certificate resources
kubectl apply -k deploy/kustomize/base/temporal-helm

# Wait for cert-manager to issue the three Temporal TLS secrets
kubectl get secret server-interservice-certs server-cloud-certs server-site-certs -n temporal

# Install via the Helm chart vendored in this repo
helm install temporal temporal-helm/temporal \
  --namespace temporal \
  --values temporal-helm/temporal/values-kind.yaml

# Create cloud and site Temporal namespaces
kubectl exec -it -n temporal deployment/temporal-admintools -- \
  temporal operator namespace create cloud --address temporal-frontend.temporal:7233
kubectl exec -it -n temporal deployment/temporal-admintools -- \
  temporal operator namespace create site --address temporal-frontend.temporal:7233
```

---

## 7. Run Database Migrations

```bash
kubectl kustomize --load-restrictor LoadRestrictionsNone \
  deploy/kustomize/overlays/db | kubectl apply -f -

kubectl wait --for=condition=complete job/nico-rest-db-migration \
  -n nico-rest --timeout=120s
```

---

## 8. Deploy NICo REST Workloads

```bash
# Site CRD must be applied before site-manager
kubectl apply -f deploy/kustomize/base/site-manager/site-crd.yaml

kubectl kustomize --load-restrictor LoadRestrictionsNone \
  deploy/kustomize/overlays/site-manager | kubectl apply -f -

kubectl kustomize --load-restrictor LoadRestrictionsNone \
  deploy/kustomize/overlays/api | kubectl apply -f -

kubectl kustomize --load-restrictor LoadRestrictionsNone \
  deploy/kustomize/overlays/workflow | kubectl apply -f -

kubectl kustomize --load-restrictor LoadRestrictionsNone \
  deploy/kustomize/overlays/site-agent | kubectl apply -f -
```

---

## Verify

```bash
kubectl get pods -n nico-rest
kubectl get pods -n temporal
kubectl get pods -n postgres
```

The API is available at `http://<node-ip>:30388` (NodePort) or `nico-rest-api.nico-rest:8388` within the cluster.

```bash
curl http://<node-ip>:30388/healthz
```

---

## Distributed Tracing (OpenTelemetry)

Every REST Go binary shares one OpenTelemetry bootstrap
(`rest-api/common/pkg/otel`). It installs an OTLP span exporter configured by the
standard `OTEL_*` environment variables, and it installs the W3C trace-context
propagator whether or not spans are exported. All settings are read once at
process start, so changing any of them requires a pod restart.

### Which services export, and what turns it on

| Service | Export requires |
|---|---|
| `nico-rest-api` | `tracing.enabled: true` in its config **and** an OTLP endpoint variable |
| `nico-rest-cloud-worker`, `nico-rest-site-worker` | `tracing.enabled: true` in the workflow config **and** an OTLP endpoint variable |
| `nico-rest-cert-manager`, `nico-rest-site-manager`, `nico-rest-site-agent`, `nico-flow`, `nico-ipam`, `nico-nvswitch-manager`, `nico-powershelf-manager` | an OTLP endpoint variable alone |

"An OTLP endpoint variable" means `OTEL_EXPORTER_OTLP_ENDPOINT` or
`OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` is set to a non-empty value. The API and
workflow config keys are:

| Key | Default | Meaning |
|---|---|---|
| `tracing.enabled` | `false` in the binary, `true` in the Helm charts | Export spans. With no endpoint variable the service logs that no provider was installed and runs untraced. |
| `tracing.serviceName` | `nico-rest-api` / `nico-rest-workflow` in the charts | `service.name` used when `OTEL_SERVICE_NAME` is not set. `OTEL_SERVICE_NAME` always wins. |

When export is off, or a service has no endpoint configured, the service still
reads inbound `traceparent`/`tracestate` headers and forwards them on its own
HTTP, gRPC, and Temporal calls, so an untraced hop does not split a trace. Set
`OTEL_PROPAGATORS=none` to turn that off as well.

### Environment variables

| Variable | Default | Notes |
|---|---|---|
| `OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` | unset | Either one enables export. The traces-specific variable wins. For gRPC use the collector's 4317 port; for HTTP use 4318 (the HTTP exporter appends `/v1/traces` to the general endpoint). |
| `OTEL_EXPORTER_OTLP_PROTOCOL`, `OTEL_EXPORTER_OTLP_TRACES_PROTOCOL` | `http/protobuf` | `grpc` or `http/protobuf`. Anything else fails the bootstrap. Set `grpc` explicitly when pointing at a 4317 endpoint. |
| `OTEL_EXPORTER_OTLP_HEADERS`, `OTEL_EXPORTER_OTLP_INSECURE`, `OTEL_EXPORTER_OTLP_CERTIFICATE`, `OTEL_EXPORTER_OTLP_TIMEOUT`, `OTEL_EXPORTER_OTLP_COMPRESSION` | exporter defaults | Read by the OTLP exporter itself; the `_TRACES_` variants also apply. |
| `OTEL_SERVICE_NAME` | unset | Overrides `tracing.serviceName` and the built-in names of the env-only services. |
| `OTEL_RESOURCE_ATTRIBUTES` | unset | Extra resource attributes, for example `service.namespace=nico-rest,deployment.environment=prod`. |
| `OTEL_PROPAGATORS` | `tracecontext` | Comma-separated list understood by the OpenTelemetry Go `autoprop` package (`tracecontext`, `baggage`, `b3`, `b3multi`, `jaeger`, `xray`, `ottrace`, `none`). Baggage is opt-in and is never carried through Temporal workflow headers. An unknown name fails the bootstrap. |
| `OTEL_TRACES_SAMPLER`, `OTEL_TRACES_SAMPLER_ARG` | SDK default, `parentbased_always_on` | Read by the SDK. Set for example `parentbased_traceidratio` with `0.1` to keep 10% of new root traces. |
| `OTEL_BSP_MAX_QUEUE_SIZE` | `2048` | Completed spans waiting for export; new spans are dropped, never blocked, when it is full. Accepted: 1 to 16384. |
| `OTEL_BSP_MAX_EXPORT_BATCH_SIZE` | `512` | Spans per export request. Accepted: 1 to 2048, and at most the queue size. |
| `OTEL_BSP_SCHEDULE_DELAY` | `5000` | Milliseconds between exports. Accepted: 100 to 10000. |
| `OTEL_BSP_EXPORT_TIMEOUT` | `30000` | Milliseconds allowed per export. Accepted: 1000 to 60000. |

A value outside the `OTEL_BSP_*` bounds, an unknown protocol, or an unknown
propagator makes the bootstrap return an error. The service logs the error and
starts without tracing rather than refusing to run.

Database queries get one span each, attached only while export is on. The span
records the SQL statement as a template with `?` placeholders; bound parameter
values are never exported.

### Helm

The `nico-rest-api`, `nico-rest-workflow`, `nico-rest-cert-manager`, and
`nico-rest-site-manager` charts each expose an `extraEnv` map (name to value)
that is rendered into the container environment. The workflow chart also has
`cloudWorker.extraEnv` and `siteWorker.extraEnv`, merged over the shared map
with the worker-specific keys winning. Neither chart ships a collector address,
so tracing stays off until you add one:

```yaml
nico-rest-api:
  config:
    tracing:
      enabled: true
      serviceName: nico-rest-api
  extraEnv:
    OTEL_EXPORTER_OTLP_PROTOCOL: grpc
    OTEL_EXPORTER_OTLP_ENDPOINT: http://otel-collector.observability.svc.cluster.local:4317
    OTEL_RESOURCE_ATTRIBUTES: service.namespace=nico-rest,deployment.environment=prod

nico-rest-workflow:
  config:
    tracing:
      enabled: true
      serviceName: nico-rest-workflow
  extraEnv:
    OTEL_EXPORTER_OTLP_PROTOCOL: grpc
    OTEL_EXPORTER_OTLP_ENDPOINT: http://otel-collector.observability.svc.cluster.local:4317
    OTEL_RESOURCE_ATTRIBUTES: service.namespace=nico-rest,deployment.environment=prod
  cloudWorker:
    extraEnv:
      OTEL_SERVICE_NAME: nico-rest-cloud-worker
  siteWorker:
    extraEnv:
      OTEL_SERVICE_NAME: nico-rest-site-worker

nico-rest-cert-manager:
  extraEnv:
    OTEL_EXPORTER_OTLP_PROTOCOL: grpc
    OTEL_EXPORTER_OTLP_ENDPOINT: http://otel-collector.observability.svc.cluster.local:4317

nico-rest-site-manager:
  extraEnv:
    OTEL_EXPORTER_OTLP_PROTOCOL: grpc
    OTEL_EXPORTER_OTLP_ENDPOINT: http://otel-collector.observability.svc.cluster.local:4317
```

`extraEnv` may not override the variables the charts manage themselves
(`CONFIG_FILE_PATH`, and for the workers `TEMPORAL_NAMESPACE` and
`TEMPORAL_QUEUE`); rendering fails if it does.

### Kustomize

The bases in `deploy/kustomize/base/` set `tracing.enabled: false` in the API
and workflow config maps and add no `OTEL_*` variables. To trace a Kustomize
deployment, patch the config map to `tracing.enabled: true` in your overlay and
add the same `OTEL_*` variables to each workload's container `env`.

### Verify

Point every service at the same collector, then exercise a request and look in
your tracing backend for spans whose `service.name` matches the values above.
A single API request that starts a workflow should appear as one trace spanning
the API server span, the Temporal client and worker spans, and the database
spans beneath them.

---

## Next Steps

- **Site agent bootstrap** — register a site via the API and configure the site agent with the resulting UUID and OTP. See [INSTALLATION.md — Step 13](INSTALLATION.md#step-13--deploy-nico-rest-site-agent).
- **Production hardening** — change default credentials, replace `start-dev` Keycloak mode, tune Temporal resource limits. See [INSTALLATION.md](INSTALLATION.md) for per-component configuration details.
- **CLI** — install `nicocli` to interact with the deployed cluster. See [cli/README.md](../cli/README.md).
