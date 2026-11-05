# NVLink Partitioning

NVIDIA [NVLink](https://www.nvidia.com/en-us/data-center/nvlink/) is a high-speed interconnect technology that allows for memory-sharing between GPUs. Sharing
is allowed between all GPUs in an *NVLink Partition*. An *NVLink Partition* must consist of GPUs within the same *NVLink Domain*, which can be a single NVL72 rack or two NVL36 racks cabled together.

NVIDIA Infra Controller (NICo) allows you to do the following with NVLink:

* Create, update, and delete NVLink Logical Partitions using the NICo REST API.
* Provision Instances with GPUs partitioned into NVLink Domains by way of NVLink Logical Partitions without knowledge of the underlying NVLink topology.
* Update Instances to change NVLink Logical Partition assignment for its GPUs

NICo extends the concept of an *NVLink Partition* with the *NVLink Logical Partition*, which allows users to manage NVLink Partitions without having to learn the datacenter topology.

> **Note**: NVLink Partitioning is only supported for GB200 compute nodes.

## Operations: Who Does What

NVLink splits between operator site setup against NMX-M / NMX-C and tenant
partition management. Notably, several operator steps (NMX-C endpoint
registration, the GPU-mapping populate step) are **not exposed via the REST
API** and are therefore driven through `nico-admin-cli` over gRPC. See
[Network Isolation → Who configures what, and how](network_isolation.md#who-configures-what-and-how)
for the role and interface model.

| Task | Role | Interface |
|---|---|---|
| Enable NVLink; NMX-M / NMX-C connection and TLS settings | Operator | **TOML** (`[nvlink_config]`) — Day 0 / rare |
| NMX-M credentials | Operator | `nico-admin-cli credential add-nmx-m` (gRPC) — not in REST |
| NMX-C endpoints (per chassis serial) | Operator | `nico-admin-cli nvlink-nmxc-endpoints …` (gRPC) — not in REST |
| Populate the machine → NMX-M GPU mapping | Operator | `nico-admin-cli nvlink-info populate` (gRPC) — not in REST |
| Create / update / delete an NVLink Logical Partition | Tenant | **REST** `…/nico/nvlink-logical-partition` · `nicocli nvlink-logical-partition create` |
| Assign or change an instance's GPUs' partition | Tenant | **REST** `…/nico/instance` (`nvLinkInterfaces`) · `nicocli instance update` |
| Inspect a machine's GPU domain placement (triage) | Operator | `nico-admin-cli machine nvlink-info` (gRPC) |

The operator NMX setup (the first four rows) is detailed under
[Enabling NMX-C-based NVLink Partitioning](#enabling-nmx-c-based-nvlink-partitioning)
and [Enabling NMX-M-based NVLink Partitioning](#enabling-nmx-m-based-nvlink-partitioning).
The tenant rows are the REST / `nicocli` flow described next.

### Creating a NVLink Logical Partition

NICo users can create NVLink Logical Partitions and plan GPU assignments using NVLink Interfaces for Instances (as described in steps **1-2**). NICo can also automatically generate NVLink Interfaces and assign them to Instances (as described in step **3**).

In general, the steps are:

1. The user creates a NVLink Logical Partition using the `POST /v2/org/{org}/nico/nvlink-logical-partition` [REST API endpoint](https://docs.nvidia.com/infra-controller/rest-api-reference/api-reference/nvlink-logical-partition/create-nvlink-logical-partition). NICo creates an entry in the database and returns an NVLink Logical Partition ID. At this point, there is no underlying NVLink Partition associated with the NVLink Logical Partition.

2. When creating an Instance, the user specifies NVLink Interface configuration for each GPU by referencing their preferred NVLink Logical Partition ID in the `POST /v2/org/{org}/nico/instance` [REST API endpoint request](https://docs.nvidia.com/infra-controller/rest-api-reference/api-reference/instance/create-instance).

   a. If this is the first Instance to be added to specified NVLink Logical Partitions, NICo Core will create and assign NVLink Partitions for them and add the Instance GPUs to the NVLink Partitions.

> **Note**: To ensure that machines in the same Rack are assigned to the same NVLink Partition, an Instance Type can be created for the Rack and all Machines in the Rack assigned to the same Instance Type. Alternatively users can use the [Batch Instance creation REST API endpoint](https://docs.nvidia.com/infra-controller/rest-api-reference/api-reference/instance/batch-create-instances) and set `topologyOptimized` to `true`.

3. If the user does not want to specify NVLink Interfaces for each GPU when creating an Instance, they can:

   a. Create a new VPC by specifying a value for `nvLinkLogicalPartitionId` or update an existing VPC with no Instances to set the `nvLinkLogicalPartitionId` attribute. We will refer to this as the *default NVLink Logical Partition* for the VPC.

   b. When creating an Instance in this VPC, user does not need to specify NVLink Interfaces, NICo will automatically generate NVLink Interfaces for the Instance and assign them to the VPC's NVLink Logical Partition.

   c. All Instances created within this VPC will have their GPUs assigned to the same NVLink Partition as long as the Instances end up in the same Rack.

   d. If there is no space in the Rack where the NVLink Partition for an NVLink Logical Partition is located, NICo will create a new NVLink Partition in a different Rack for the same NVLink Logical Partition and continue to assign the Instance GPUs to it.

> **Important**: If Instances are in different Racks, they will not be able to share memory with each other despite having the same NVLink Logical Partition.

### Updating an Instance to change NVLink Logical Partition assignment for its GPUs

If a NICo user wants to update an Instance to change NVLink Logical Partition assignment for its GPUs, they can do so by calling the `PATCH /v2/org/{org}/nico/instance/{instance-id}` [REST API endpoint](https://docs.nvidia.com/infra-controller/rest-api-reference/api-reference/instance/update-instance)

The user can specify the NVLink Logical Partition ID for each GPU in the Instance by passing the `nvLinkInterfaces` list.

If Instance's VPC has a default NVLink Logical Partition, no changes to the NVLink Logical Partition assignment are allowed.

### Removing Instances from a Logical Partition

If a user de-provisions an Instance, NICo will remove the Instance GPUs from the NVLink Partition.

### Deleting an NVLink Logical Partition

A NICo user can call `DELETE /v2/org/{org}/nico/nvlink-logical-partition/{nvLinkLogicalPartitionId}` to delete an NVLink Logical Partition. This call will only succeed if there are no active Instances associated with the NVLink Logical Partition.

### Retrieving NVLink Partition Information for an Instance

A NICo user can call `GET /v2/org/{org}/nico/instance/{instance-id}` to retrieve information about an Instance. As part of the `200` response body, NICo will return a `nvLinkInterfaces` list that includes both the `nvLinkLogicalPartitionId` and `nvLinkDomainId` for each GPU in the Instance.

### Default NVLink Logical Partition for a VPC

**It's an optional default, not a constraint.**
VPCs can be created with or without a default NVLink Logical Partition.

It is optional on both `POST .../vpc` (Create VPC) and `PATCH .../vpc/{vpcId}` (Update VPC).

**What setting it on a VPC actually does**
It's a convenience default for instance creation. When `nvLinkLogicalPartitionId` is set on the VPC, you don't have to specify `nvLinkInterfaces` on `POST .../instance` (Create Instance) or `POST .../instance/batch` (Batch Create Instances) — the API will auto-populate the per-GPU NVLink Interfaces to reference that VPC's NVLink Logical Partition.
That's the entire effect. It does not reserve or lock the NVLink Logical Partition to the VPC.

**No exclusivity between VPCs**
We intentionally don't restrict an NVLink Logical Partition to a single VPC. The same `nvLinkLogicalPartitionId` may be set on multiple VPCs. This is deliberate, to preserve flexibility in how you plan networking and NVLink partitioning.

**You can also manage NVLink at the Instance level**
If you want finer control, leave `nvLinkLogicalPartitionId` unset on the VPC and specify `nvLinkInterfaces` directly on Create Instance — each entry binds a specific `deviceInstance` (GPU) to an explicit `nvLinkLogicalPartitionId`, so different GPUs in the same instance (or across Instances in the same VPC) can operate in different NVLink Logical Partitions.

**Summary**
| Configuration | Behavior |
| --- | --- |
| VPC has `nvLinkLogicalPartitionId`, Instance creation omits `nvLinkInterfaces` | API auto-populates all GPUs to the VPC's NVLink Logical Partition |
| VPC has `nvLinkLogicalPartitionId`, Instance specifies `nvLinkInterfaces` | Instance-level values must align with VPC's Partition, rendering the specification redundant |
| VPC doesn't have `nvLinkLogicalPartitionId` set, Instance specifies `nvLinkInterfaces` | Per-GPU NVLink Logical Partition assignments are used |
| Same `nvLinkLogicalPartitionId` on multiple VPCs | Allowed — no implicit exclusivity |

### How NICo Reconciles NVLink State

NICo runs a periodic reconciler against NMX-M and NMX-C to keep the actual
NVLink partition topology aligned with the desired state implied by tenant
instance configurations. The behaviour matters whenever an operator is
diagnosing latency between an API call and an instance becoming `Ready`.

Each reconciliation pass does the following:

1. Loads every NVLink Logical Partition and every NVLink Physical Partition
   from the NICo database.
2. Queries each configured NMX-M and / or NMX-C endpoint for the current
   partition list and GPU membership on that endpoint's chassis or domain.
3. Compares observed state against desired state.
4. Issues create / update / remove operations to the fabric-management
   service to converge it onto desired state.
5. Updates per-machine GPU status observations in the NICo database. The
   per-instance `configs_synced.nvlink` field is derived from these
   observations and is what gates the instance's `Ready` state.

Cadence is set by `nvlink_config.monitor_run_interval` (default `60s`).

#### Metrics

The reconciler exposes metrics under the
`carbide_nvlink_partition_monitor_*` namespace. Useful ones:

| Metric | Use | `health` values |
|---|---|---|
| `carbide_nvlink_partition_monitor_iteration_latency_milliseconds` | Time per reconcile pass | |
| `carbide_nvlink_partition_monitor_nmxc_op_latency_milliseconds` | Per-operation latency against NMX-C | |
| `carbide_nvlink_partition_monitor_nmxc_changes_applied_total` | Counter of changes issued; nonzero in steady state is an anomaly | |
| `carbide_nvlink_partition_monitor_nmxc_connect_error_count` | Connection failures to any NMX-C endpoint | |
| `carbide_nvlink_partition_monitor_num_logical_partitions` | Logical-partition count NICo is tracking | |
| `carbide_nvlink_partition_monitor_num_physical_partitions` | Physical-partition count NICo is tracking | |
| `carbide_nvlink_partition_monitor_nmxc_partition_count` | Partition count NMX-C reports, by `nvlink_domain_uuid` and `health` | `healthy`, `degraded_bw`, `degraded`, `unhealthy`, `unknown` |
| `carbide_nvlink_partition_monitor_nmxc_gpu_count` | GPU count NMX-C reports, by `nvlink_domain_uuid` and `health` | `healthy`, `degraded`, `no_nvlink`, `degraded_bw`, `unknown` |
| `carbide_nvlink_partition_monitor_nmxc_compute_node_count` | Compute-node count NMX-C reports, by `nvlink_domain_uuid` and `health` | `healthy`, `degraded`, `unhealthy`, `unknown` |

### Instance Release and Logical Partition Deletion

When an instance is released (via `ReleaseInstance`):

1. The instance's NVLink configuration is cleared from the database.
2. The reconciler observes that GPUs previously assigned to the instance
   are no longer requested in any live partition.
3. The reconciler removes those GPUs from their NMX-M / NMX-C partitions.
4. Once all NVLink state is removed, the machine's GPU status observation
   reflects an empty domain assignment and the host becomes eligible for
   reuse.

When a Logical Partition is deleted, every underlying NVLink Physical
Partition on each NMX-M / NMX-C endpoint backing it is also deleted. The
deletion is rejected if any instance still references the Logical
Partition.

When a host is force-deleted, the instance running on it is implicitly
released. The monitor only removes GPUs from partitions for hosts that NICo
still manages, so it removes them once the host is rediscovered. If the
host's compute tray is physically moved or replaced instead, isolate the tray
first, as described in
[Moving or Replacing a Compute Tray](#moving-or-replacing-a-compute-tray).

### Moving or Replacing a Compute Tray

Remove a compute tray's GPUs from every NVLink partition before the tray leaves
its slot. NMX-C tracks partition membership by GPU UID and by slot location. If
a tray is moved or replaced while its GPUs are still in a partition, NMX-C keeps
the old UIDs as members but gives the slot's membership to the GPUs installed
next. It then rejects every create, add, and remove request that would reconcile
the two. The only known recovery is an NMX-C factory reset, which disrupts every
tray in the NVLink domain.

1. Mark the host with the `compute-tray-due-for-swap` template:

   ```bash
   nico-admin-cli machine health-report add <machine-id> \
     --template compute-tray-due-for-swap --message "<ticket>"
   ```

   The template blocks new allocations and carries the
   [`IsolateFromNvLinkPartitions`](../architecture/health/health_alert_classifications.md#isolatefromnvlinkpartitions)
   classification. On its next pass, the monitor removes every GPU on the host
   from every NVLink partition, tenant or tray default, and keeps them out while
   the mark remains. It does so even if the host has an instance, which cuts
   that instance's NVLink connectivity to the rest of its partition.

1. Wait until the monitor reports `NvlinkIsolated` under the `nvlink-isolation`
   health report source:

   ```bash
   nico-admin-cli -f json machine health-report show <machine-id> \
     | jq -r '.[].report | select(.source == "nvlink-isolation") | (.successes[]?.id, .alerts[]?.message)'
   ```

   `NvlinkIsolated` means NMX-C lists none of the host's GPUs in any partition.
   Any other output, including no output, means the tray must stay in its slot:
   a message names the GPUs still in a partition or states that isolation
   cannot be confirmed, and no output means the monitor has not checked the host
   since it was marked. For details, see
   [NVLink isolation health probe identifiers](../architecture/health/health_probe_ids.md#nvlink-isolation-health-probe-identifiers).

1. Keep the mark until the tray is out of its slot, and force-delete the host
   only after that. Force-deletion removes the mark, and while the tray is
   still installed, Site Explorer can rediscover it as an unmarked host whose
   GPUs the monitor returns to `tray_partition_<slot>`.

Removing the tray and ingesting its replacement follow the site's hardware
replacement process.

### Enabling NMX-C-based NVLink Partitioning

NMX-C is the gRPC control path for NVLink partition management and is the
current default for new deployments. NMX-M remains supported and is
covered in the next section; the two are not mutually exclusive and a
single site may use both.

The TOML toggles live alongside the NMX-M ones under `[nvlink_config]`:

```toml
[nvlink_config]
enabled = true
monitor_run_interval = "60s"

# Optional TLS material for NMX-C. Leave unset to use the system trust
# store and present no client certificate.
nmx_c_tls_ca_cert_path     = "/etc/nico/nmxc/ca.pem"
nmx_c_tls_client_cert_path = "/etc/nico/nmxc/client.crt"
nmx_c_tls_client_key_path  = "/etc/nico/nmxc/client.key"
nmx_c_tls_authority        = "nmxc.example.internal"

allow_insecure = false
```

| Field | Purpose |
|---|---|
| `nmx_c_tls_ca_cert_path` | Optional PEM containing additional CAs for verifying the NMX-C endpoint's certificate |
| `nmx_c_tls_client_cert_path` | Optional client certificate for mTLS to NMX-C |
| `nmx_c_tls_client_key_path` | Optional client key matching the certificate above |
| `nmx_c_tls_authority` | Optional override for the expected server name during certificate verification (SNI / hostname check) |
| `allow_insecure` | When `true`, disables TLS verification entirely. Intended for development |

Unlike NMX-M, where a single endpoint URL is set in TOML, NMX-C endpoints
are **per-chassis** and stored in the NICo database. Register them with
`nico-admin-cli`, keyed by the chassis serial:

```bash
nico-admin-cli nvlink-nmxc-endpoints create \
    --chassis-serial <serial> \
    --endpoint https://nmxc-host:443

nico-admin-cli nvlink-nmxc-endpoints show
```

`update` and `delete` subcommands follow the same pattern. The reconciler
picks up new endpoints on the next iteration; no restart is required.

The TLS material in TOML applies uniformly to every NMX-C endpoint NICo
talks to. Per-endpoint credential overrides are not currently supported;
deploy a uniform trust posture across the site's NMX-C control plane.

### Enabling NMX-M-based NVLink Partitioning

This section describes how to enable NVLink support via the [NMX-M platform](https://docs.nvidia.com/networking/display/nmxmv8513000).

#### Prerequisites

* nico-core/NICo is deployed and running.
* vault is running.
* nico-core can reach the NMX-M endpoint over the network.
* NMX-M has an API user with permissions to read GPUs/partitions and create/update/delete partitions.

#### Steps to Enable NMX-M

1. Enable NVLink Partitioning in nico-core config. Add or update the configmap nico-api-site-config-files consumed by nico-core:

    ```
      [nvlink_config]
      enabled = true
      nmx_m_endpoint = "https://<nmx-m-host>:<port>"
    
      monitor_run_interval = "60s"
      nmx_m_operation_timeout = "10s"
    
      allow_insecure = true
    ```

2. Restart nico-core

3. Configure the NMX-M credentials. Store the NMX-M username and password in vault through nico admin CLI:

    ```
      nico-admin-cli credential add-nmx-m \
        --username <nmx-m-username> \
        --password <nmx-m-password>
    ```

4. Populate the NVLink GPU mapping. After enabling NVLink in the site config, for already discovered machines, populate the machine-to-NMX-M GPU mapping. Partitioning will not work until this step is complete.

   <Note>Machines discovered after enabling NVLink do not require this step.</Note>

    ```
    nico-admin-cli nvlink-info populate --update-db <machine-id>
    ```

5. Validate the NVLink configuration for NMX-M:

    * nico-core logs should not show "Failed to create NMXM client".
    * Logs should not show failures getting NMX-M partitions or GPU list.
    * Metrics show that `nico_nvlink_partition_monitor_nmxm_connect_error_count` is `0`.

### Verifying a Tenant's NVLink Placement

After an instance has been created and the reconciler has had at least one
opportunity to run, an operator can confirm correct placement with the
following checks. There is no single all-in-one health command; the steps
below should be repeatable as a checklist.

1. **Reconciler is running.**
   `nico_nvlink_partition_monitor_iteration_latency` is being recorded
   and both `nico_nvlink_partition_monitor_nmxc_connect_error_count`
   and `nico_nvlink_partition_monitor_nmxm_connect_error_count` are
   flat.
2. **Logical-partition count matches expectation.**
   `nico_nvlink_partition_monitor_num_logical_partitions` reflects
   the partitions a site planner expects to exist. A sudden change is
   worth correlating with recent tenant API activity.
3. **Per-instance configuration has converged.** The instance's
   `InstanceStatus` reports `configs_synced.nvlink = true` and the
   `nvLinkInterfaces` list on the instance shows the expected
   `nvLinkLogicalPartitionId` and `nvLinkDomainId` for each GPU.
4. **Per-machine GPU placement.**

    ```
    nicocli machine nvlink-info --machine-id <machine-id>
    ```

    Returns the machine's NVLink GPU status observations, including the
    Domain each GPU is currently assigned to. Use this to confirm that
    two instances expected to share an NVLink Logical Partition have
    actually landed in the same NVLink Domain — instances in different
    Domains cannot share GPU memory regardless of having the same
    Logical Partition ID.
5. **Cleanup after release.** After releasing an instance, the same
   `machine nvlink-info` output should show an empty Domain assignment
   on the affected GPUs within one or two reconcile intervals. Failure
   to clear indicates the reconciler could not remove the GPU from its
   NMX-M / NMX-C partition; investigate the corresponding connect-error
   and op-latency metrics.
