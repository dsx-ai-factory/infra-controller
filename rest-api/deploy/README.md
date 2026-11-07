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

Tracing for the REST services is documented in
[REST service tracing](../../docs/observability/tracing.md#7-rest-service-tracing):
which services export, the `tracing.*` config keys and `OTEL_*` variables, and
the Helm and Kustomize wiring.

---

## Next Steps

- **Site agent bootstrap** — register a site via the API and configure the site agent with the resulting UUID and OTP. See [INSTALLATION.md — Step 13](INSTALLATION.md#step-13--deploy-nico-rest-site-agent).
- **Production hardening** — change default credentials, replace `start-dev` Keycloak mode, tune Temporal resource limits. See [INSTALLATION.md](INSTALLATION.md) for per-component configuration details.
- **CLI** — install `nicocli` to interact with the deployed cluster. See [cli/README.md](../cli/README.md).
