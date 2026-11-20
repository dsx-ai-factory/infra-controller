# Development Keycloak

This chart deploys Keycloak for an isolated local development stack. It uses
`start-dev` and the public bootstrap administrator credentials `admin` / `admin`;
it is not a production identity-provider configuration. PostgreSQL, its database
and role, and the release namespace must already exist. Database provisioning and
NICo site registration are separate operations.

Tilt supplies the canonical
[`nico-dev` realm](../../rest-api/deploy/kustomize/base/keycloak/nico-dev-realm.json),
also consumed by Kustomize. The reference deployment's `nico` realm is a different
fixture; this chart does not change its setup workflow.

## Direct Helm installation

For the standard local stack's existing database and namespace, run from the
repository root with that cluster's kubeconfig selected:

```bash
helm upgrade --install keycloak helm/nico-dev-keycloak \
  --namespace nico-rest --wait --timeout 5m \
  --set database.url=jdbc:postgresql://nico-pg-cluster-rw.nico-system.svc.cluster.local:5432/keycloak \
  --set database.password=keycloak_password \
  --set-file 'realmFiles.nico-dev-realm\.json=rest-api/deploy/kustomize/base/keycloak/nico-dev-realm.json'
```

The URL and explicit password above match the standard local stack. Override
the connection and credentials in a private values file for another database.
Helm stores supplied values in its release Secret. The realm JSON contains public development users and
client credentials; use private import files outside the isolated local stack.

For reconciliation of a Tilt-installed release using its saved values:

```bash
helm upgrade keycloak helm/nico-dev-keycloak \
  --namespace nico-rest --reuse-values --wait --timeout 5m
```

## Values and ownership

| Value | Default | Contract |
| --- | --- | --- |
| `image` | Digest-pinned multi-architecture Keycloak 24.0 image in `values.yaml` | Complete image reference. This release's probes and import behavior are validated with that image on ARM64 and AMD64. |
| `database.url` | `jdbc:postgresql://postgres.postgres:5432/keycloak` | Nonempty JDBC connection URL for an existing PostgreSQL database. |
| `database.username` | `keycloak` | Nonempty existing database role. |
| `database.password` | `keycloak` | Nonempty password matching that role. |
| `realmFiles` | `{}` | Map of ConfigMap-compatible filenames to JSON import strings. Use `<realm>-realm.json` for realm exports with associated user files; see [Keycloak import conventions](https://www.keycloak.org/server/importExport). Startup also imports other regular `.json` files as single-file exports, excluding names containing `-realm.json` or `-users-`. Non-JSON files are ignored. Empty imports no realm. Helm writes the strings literally; Keycloak interprets them at startup. |
| `service.nodePort.enabled` | `true` | Create `keycloak-nodeport` in addition to the ClusterIP Service. |
| `service.nodePort.port` | `30082` | NodePort accepted by the cluster and available on its nodes; ignored when disabled. |

The release owns fixed names `keycloak` (Deployment and ClusterIP Service),
`keycloak-nodeport` (optional Service), `keycloak-realm` (ConfigMap), and
`keycloak-db-credentials` (Secret with `username` and `password`). Only one release
can own these names in a namespace. Both Services listen on 8082 and target
container port 8080. The Deployment mounts the ConfigMap for `--import-realm`
and reads database credentials through Secret references; it does not need a
registry pull Secret for the default public image.

Existing raw resources are not adopted; use a fresh development cluster for that
ownership transition. Same-input upgrades retain the Service addresses and
workload configuration. Changing database credentials rolls the workload through a pod-template
checksum, but does not change the PostgreSQL role; coordinate the database
change before updating the chart.

Realm import only creates missing realms. Updating the ConfigMap or restarting
Keycloak does not overwrite an existing realm: use the Keycloak administration
API or console to update it. The database retains users, clients, realm keys and
role mappings across workload replacement. Removing the release interrupts
authentication and deletes its Kubernetes objects, while leaving the external
database intact. Restore with the same database connection and credentials;
no Helm hook creates or deletes database data or registers NICo sites.
