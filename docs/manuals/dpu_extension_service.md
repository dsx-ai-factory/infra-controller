# DPU Extension Service Management

DPU Extension Services let tenants deploy and manage custom workloads on the DPUs attached to their instances.

NVIDIA Infra Controller (NICo) allows you to do the following with DPU Extension Services:

* Create, update, list and delete reusable service definitions using REST API or admin CLI.
* Deploy one or more service versions to the DPUs attached to an instance.
* Upgrade or remove deployed services by updating the Instance configuration.
* Monitor deployment status of the extension services on the Instance through instance status report.

Currently, the only supported service type is **Kubernetes Pod** (`KubernetesPod`). For this service type, the service `data` field must contain a Kubernetes Pod manifest in YAML format.

## Typical Workflow

1. Create a DPU Extension Service with a Pod manifest (and optional registry credentials).
2. Deploy the service by referencing it in instance config's extension service config when creating or updating an Instance.
3. Monitor deployment status on the Instance until all DPUs report `Running`.
4. Upgrade by creating a new service version, then updating each affected Instance to reference the new version.
5. When deleting an instance, DPU extension services will be automatically removed from attached DPUs.

> **Important:** Creating or updating a service definition does not automatically redeploy running Instances. To roll out a new version, update the Instance's `dpuExtensionServiceDeployments`.

## Core Concepts

| Concept | Description |
|---|---|
| **DPU Extension Service** | A reusable tenant-owned service definition with a type, name, description, and one or more versions. |
| **Version** | An immutable snapshot of the Pod manifest (`data`), optional registry credentials, and optional observability configuration. Versions use NICo config version strings (for example, `V1-T1761856992374052`). |
| **Deployment** | An association between an Instance and a specific `(service_id, version)` pair. |
| **Active versions** | All non-deleted versions available for deployment. Older versions remain until explicitly deleted. |
| **Terminating services** | When a service is removed from an Instance, NICo tracks it until the affected DPUs confirm termination. |

**Important Constraints:**

* Service names must be case-insensitive unique among non-deleted services within a tenant. After a service is deleted, a new service in the same tenant may reuse the same name.
* Service type and tenant ownership are immutable after creation.
* An Instance may deploy multiple services but at most one version of each service at a time.
* Instance extension service changes are only accepted while the Instance is in `Ready` state.

## Managing DPU Extension Services

### Create an DPU Extension Service

Create a service definition before deploying it to an Instance.

To create a service definition of type `KubernetesPod`, prepare a Pod manifest according to [Kubernetes Pod requirements](#kubernetes-pod-requirements)

**REST API:** `POST /v2/org/{org}/nico/dpu-extension-service`

```json
{
  "name": "my-sidecar",
  "description": "Sidecar workload",
  "serviceType": "KubernetesPod",
  "siteId": "60189e9c-7d12-438c-b9ca-6998d9c364b1",
  "data": "apiVersion: v1\nkind: Pod\nmetadata:\n  name: my-sidecar\nspec:\n  containers:\n    - name: app\n      image: busybox:latest\n      command: [\"sh\", \"-c\", \"sleep 3600\"]"
}
```

**Admin CLI:**

```bash
nico-admin-cli extension-service create \
  --id <<some UUID>>  \
  --name test1 \
  --type 0 \
  --tenant-organization-id test_org \
  --data "$(< test_pod.txt)" \
```

To include private registry credentials, add a `credentials` object to the request body. See [Registry credentials](#registry-credentials). All three credential flags (`--registry-url`, `--username`, `--password`) are required tenant wants to supply a credential.

```bash
# With private registry credentials
nico-admin-cli extension-service create \
  --id <service_uuid>  \
  --name test1 \
  --type 0 \
  --tenant-organization-id <org_id> \
  --data "$(< test_pod.txt)" \
  --username "$oauthtoken" \
  --password <registry_token> \
  --registry_url "nvcr.io/<org_name>" 
```

### List DPU Extension Services

**REST API:** `GET /v2/org/{org}/nico/dpu-extension-service`

Optional query parameters: `siteId`, `status`, `query`, `includeRelation`, `pageNumber`, `pageSize`, and `orderBy` (`NAME_ASC`, `NAME_DESC`, `STATUS_ASC`, `STATUS_DESC`, `CREATED_ASC`, `CREATED_DESC`, `UPDATED_ASC`, `UPDATED_DESC`).

**Admin CLI:**

```bash
nico-admin-cli extension-service show
```

Optional filters: `--type`, `--name`, `--tenant-organization-id`.

### Get an extension service

Retrieve a single service, including its latest version and list of active versions.

**REST API:** `GET /v2/org/{org}/nico/dpu-extension-service/{dpuExtensionServiceId}`

**Admin CLI:**

```bash
nico-admin-cli extension-service show --id <service-id>
```

### Get a DPU Extension Service Version

Retrieve the Pod manifest and metadata for a specific version.

**REST API:** `GET /v2/org/{org}/nico/dpu-extension-service/{dpuExtensionServiceId}/version/{version}`

**Admin CLI:**

```bash
nico-admin-cli extension-service get-version --id <service-id> --version <version>
```

### Update a DPU Extension Service

Changes to `data`, `credentials`, or `observability` create a **new** immutable version which the latest version number of the extension service is incremented. Changes to name or description only do not create a new version, latest version number stays unchanged. Identical specs are rejected.

**REST API:** `PATCH /v2/org/{org}/nico/dpu-extension-service/{dpuExtensionServiceId}`

```json
{
  "data": "apiVersion: v1\nkind: Pod\nmetadata:\n  name: my-sidecar-v2\nspec:\n  containers:\n    - name: app\n      image: busybox:1.36\n      command: [\"sh\", \"-c\", \"sleep 3600\"]"
}
```

Optional fields: `name`, `description`, `credentials`, and `observability`. Omit `data` to update metadata only.

**Admin CLI:**

```bash
nico-admin-cli extension-service update \
  --id <service-id> \
  --data "$(cat pod-v2.yaml)" \
  --if-version-ctr-match 2
```

Use `--if-version-ctr-match` (CLI only) to prevent concurrent update conflicts.

### Delete a DPU Extension Service or Version

Deletion succeeds only when no Instance is using the version being deleted. If a version is still deployed, remove it from affected Instances first. When all versions are deleted, the service itself is removed automatically. Deleted service names are released and can be reused by new services in the same tenant.

**REST API:**

* Delete all versions: `DELETE /v2/org/{org}/nico/dpu-extension-service/{dpuExtensionServiceId}`
* Delete a specific version: `DELETE /v2/org/{org}/nico/dpu-extension-service/{dpuExtensionServiceId}/version/{version}`

**Admin CLI:**

```bash
# Delete the entire service (all versions)
nico-admin-cli extension-service delete --id <service-id>

# Delete specific versions
nico-admin-cli extension-service delete --id <service-id> --version <version1>,<version2>,...
```

---

## Managing Instance DPU Extension Service Deployments

Deployment to Instances is managed through the REST API only.

### Deploy DPU Extension Services to an Instance

Reference a service when creating or updating an Instance. If `version` is omitted, NICo deploys the latest version.

**REST API:**

* Create Instance: `POST /v2/org/{org}/nico/instance`
* Batch create: `POST /v2/org/{org}/nico/instance/batch`
* Update Instance: `PATCH /v2/org/{org}/nico/instance/{instanceId}`

Include `dpuExtensionServiceDeployments` in the request body:

```json
{
  "dpuExtensionServiceDeployments": [
    {
      "dpuExtensionServiceId": "497f6eca-6276-4993-bfeb-53cbbbba6f08",
      "version": "V1-T1761856992374052"
    }
  ]
}
```

During Instance provisioning, NICo waits for all configured extension services to reach a running state on every DPU used by the Instance before the Instance becomes ready. On multi-DPU hosts, DPUs that are attached to the host but not selected by the Instance network configuration do not block the extension-service readiness check. If no extension services are configured, this step is skipped.

### Upgrade a Deployed DPU Extension Service for an Instance

1. Update the service definition to create a new version (see [Update a DPU Extension Service](#update-a-dpu-extension-service)).
2. Update each affected Instance to reference the new version in `dpuExtensionServiceDeployments`.

The Instance remains in `Ready` state while DPUs asynchronously apply the new configuration. Monitor progress through the Instance deployment status.

### Remove DPU Extension Services from an Instance

Update the Instance and remove the entry from `dpuExtensionServiceDeployments`. NICo terminates the service on each DPU and reports `Terminating`, then `Terminated`, in the Instance status.

During Instance deallocation, NICo waits for all extension services to reach `Terminated` on every DPU before completing cleanup.

### Check Instance DPU Extension Service Deployment Status

**REST API:** `GET /v2/org/{org}/carbide/instance/{instance-id}`

The response includes `dpuExtensionServiceDeployments` with per-service deployment status. See [Deployment status values](#deployment-status-values) for status definitions.

### Find Instances using an extension service

**Admin CLI:**

```bash
nico-admin-cli extension-service show-instances --id <service-id>
```

Optionally filter by version: `--version <version>`.

---

## Reference

### Kubernetes Pod Requirements

For DPU Extension Service defined with `KubernetesPod` type, the `data` field of the DPU Extension Service must be a valid Kubernetes **Pod** manifest in YAML format.

The pod manifest must not exceed 64 KB and the pod manifest must have following fields:

* `apiVersion`
* `kind: Pod`
* `metadata.name`
* `spec.containers` — a non-empty array

**Example:**

```yaml
apiVersion: v1
kind: Pod
metadata:
  name: busybox-sidecar
spec:
  containers:
    - name: busybox-container
      image: busybox:latest
      command: ["sh", "-c", "echo 'BusyBox container running' && sleep 3600"]
```

### Registry Credentials

For DPU Extension Service defined with `KubernetesPod` type, in order for the DPU agent to pull images referenced in the pod manifest, tenant should provider credentials when creating or updating a service:

```rpc
message DpuExtensionServiceCredential {
  string registry_url = 1;
  oneof type {
    UsernamePassword username_password = 2;
  }
}
```

Credentials are kept in the NICo credential store and will not be displayed when tenant queries service definition through REST API or admin CLI. Each field must be non-empty and at most 255 characters. Deleting a service version removes its associated credentials from the store.

The registry URL is used as an image match prefix for kubelet's [image credential provider](https://kubernetes.io/docs/tasks/administer-cluster/kubelet-credential-provider/). NICo matches credentials against the image reference by prefix.

**Examples:**

* Images `nvcr.io/nvforge/abcd` and `nvcr.io/nvforge/efgh` → use `registryUrl` of `nvcr.io/nvforge` (no trailing `/`).
* Image `nvcr.io/nv-ngn/sdn/controller:latest` → use `registryUrl` of `nvcr.io/nv-ngn/sdn`.

**Multiple services with different credentials:** Each service that needs its own credentials must use a distinct `registryUrl` prefix. If two services share the same prefix (for example, both use `nvcr.io/nvforge`) with different username/password pairs, kubelet may apply either credential non-deterministically.

DPUs pull images through a site-controller SOCKS5 proxy (`socks5://socks.forge:1888`). No additional proxy configuration is required in the service definition.

### Versioning

| Action | Creates new version? |
|---|---|
| Create service | Yes — initial version `V1`, `version_ctr` set to 1 |
| Update name or description only | No |
| Update `data`, `credentials`, or `observability` | Yes — increments `version_ctr` |
| Submit identical `data` and credentials | Rejected |

Previous versions remain available for deployment until deleted. To roll out a new version, update each Instance's `dpuExtensionServiceDeployments`.

### Deployment Status Values

| Status | Meaning |
|---|---|
| `Pending` | Deployment is in progress. |
| `Running` | The service is running on the DPU. |
| `Terminating` | The service is being removed from the Instance. |
| `Terminated` | The service has been fully removed from the DPU. |
| `Failed` / `Error` | Deployment or teardown encountered an error. Check the status message on the Instance. |
| `Unknown` | Status not yet available, or DPUs have not synced the desired configuration. |

NICo aggregates status across all DPUs on an Instance. If any DPU reports `Error`, the overall status is `Error`. If any DPU reports `Pending`, the overall status is `Pending`. All DPUs must report `Running` for the overall status to be `Running`.

### Instance Lifecycle Behavior

**Provisioning:** After network configuration completes, the Instance waits in `WaitingForExtensionServicesConfig` until all configured extension services are running on every DPU. Hosts with zero DPUs skip this step.

**Configuration updates:** Extension services can be added, upgraded, or removed while the Instance is in `Ready` state. Removed services are tracked as terminating until all DPUs confirm `Terminated`.

**Deallocation:** During teardown, NICo waits in `WaitingForNetworkReconfig` until all extension services report `Terminated` before completing cleanup.
