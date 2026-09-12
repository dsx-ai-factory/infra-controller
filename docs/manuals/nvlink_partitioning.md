# NVLink Partitioning

NVIDIA [NVLink](https://www.nvidia.com/en-us/data-center/nvlink/) is a high-speed interconnect technology that allows for memory-sharing between GPUs. Sharing
is allowed between all GPUs in an *NVLink Partition*. An *NVLink Partition* must consist of GPUs within the same *NVLink Domain*, which can be a single NVL72 rack or two NVL36 racks cabled together.

NVIDIA Infra Controller (NICo) allows you to do the following with NVLink:

* Create, update, and delete NVLink Logical Partitions using the NICo REST API.
* Provision Instances with GPUs assigned to partitions within NVLink Domains by way of NVLink Logical Partitions, without knowledge of the underlying NVLink topology.
* Update Instances to change NVLink Logical Partition assignment for its GPUs

NICo extends the concept of an *NVLink Partition* with the *NVLink Logical Partition*, which allows users to manage NVLink Partitions without having to learn the datacenter topology.

<Note title="Compatibility note">
NVLink Partitioning is only supported for GB200 compute nodes.
</Note>

## Operations: Who Does What

NVLink operations are split between operator site setup against NMX-C and
tenant partition management. Notably, one operator step (fallback NMX-C endpoint
registration) is **not exposed via the REST API** and is therefore driven
through `nico-admin-cli` over gRPC. See
[Network Isolation → Who configures what, and how](network_isolation.md#who-configures-what-and-how)
for the role and interface model.

| Task | Role | Interface |
|---|---|---|
| Enable NVLink; NMX-C connection and TLS settings | Operator | **TOML** (`[nvlink_config]`) — Day 0 / rare |
| Fallback NMX-C endpoints (per chassis serial) | Operator | `nico-admin-cli nvlink-nmxc-endpoints ...` (gRPC) — not in REST |
| Create / update / delete an NVLink Logical Partition | Tenant | **REST** `.../nico/nvlink-logical-partition` · `nicocli nvlink-logical-partition create` |
| Assign or change an instance's GPUs' partition | Tenant | **REST** `.../nico/instance` (`nvLinkInterfaces`) · `nicocli instance update` |
| Inspect a machine's GPU domain placement (triage) | Operator | `nico-admin-cli machine nvlink-info show <MACHINE_ID>` (gRPC) |

The operator NMX setup (the first two rows) is detailed under
[Enabling NMX-C-based NVLink Partitioning](#enabling-nmx-c-based-nvlink-partitioning).
The tenant rows are the REST / `nicocli` flow described next.

### Creating a NVLink Logical Partition

NICo users can create NVLink Logical Partitions and plan GPU assignments using NVLink Interfaces for Instances (as described in steps **1-2**). NICo can also automatically generate NVLink Interfaces and assign them to Instances (as described in step **3**).

In general, the steps are:

1. The user creates a NVLink Logical Partition using the `POST /v2/org/{org}/nico/nvlink-logical-partition` [REST API endpoint](api:POST/v2/org/:org/nico/nvlink-logical-partition). NICo creates an entry in the database and returns an NVLink Logical Partition ID. At this point, there is no underlying NVLink Partition associated with the NVLink Logical Partition.

1. When creating an Instance, the user specifies NVLink Interface configuration for each GPU by referencing their preferred NVLink Logical Partition ID in the `POST /v2/org/{org}/nico/instance` [REST API endpoint request](api:POST/v2/org/:org/nico/instance).

   a. If this is the first Instance to be added to specified NVLink Logical Partitions, NICo Core will create and assign NVLink Partitions for them and add the Instance GPUs to the NVLink Partitions.

   <Note>
   To ensure that machines in the same Rack are assigned to the same NVLink Partition, an Instance Type can be created for the Rack and all Machines in the Rack assigned to the same Instance Type. Alternatively users can use the [Batch Instance creation REST API endpoint](api:POST/v2/org/:org/nico/instance/batch) and set `topologyOptimized` to `true`.
   </Note>

1. If the user does not want to specify NVLink Interfaces for each GPU when creating an Instance, they can:

   a. Create a new VPC by specifying a value for `nvLinkLogicalPartitionId` or update an existing VPC with no Instances to set the `nvLinkLogicalPartitionId` attribute. We will refer to this as the *default NVLink Logical Partition* for the VPC.

   b. When creating an Instance in this VPC, user does not need to specify NVLink Interfaces, NICo will automatically generate NVLink Interfaces for the Instance and assign them to the VPC's NVLink Logical Partition.

   c. All Instances created within this VPC will have their GPUs assigned to the same NVLink Partition as long as the Instances end up in the same Rack.

   d. If there is no space in the Rack where the NVLink Partition for an NVLink Logical Partition is located, NICo will create a new NVLink Partition in a different Rack for the same NVLink Logical Partition and continue to assign the Instance GPUs to it.

<Warning title="Important">
If Instances are in different Racks, they will not be able to share memory with each other despite having the same NVLink Logical Partition.
</Warning>

### Updating an Instance to change NVLink Logical Partition assignment for its GPUs

If a NICo user wants to update an Instance to change NVLink Logical Partition assignment for its GPUs, they can do so by calling the `PATCH /v2/org/{org}/nico/instance/{instance-id}` [REST API endpoint](api:PATCH/v2/org/:org/nico/instance/:instanceId).

The user can specify the NVLink Logical Partition ID for each GPU in the Instance by passing the `nvLinkInterfaces` list.

If Instance's VPC has a default NVLink Logical Partition, no changes to the NVLink Logical Partition assignment are allowed.

### Removing Instances from a Logical Partition

If a user de-provisions an Instance, NICo will remove the Instance GPUs from the NVLink Partition.

### Deleting an NVLink Logical Partition

A NICo user can call [`DELETE /v2/org/{org}/nico/nvlink-logical-partition/{nvLinkLogicalPartitionId}`](api:DELETE/v2/org/:org/nico/nvlink-logical-partition/:nvLinkLogicalPartitionId) to delete an NVLink Logical Partition. This call will only succeed if there are no active Instances associated with the NVLink Logical Partition.

### Retrieving NVLink Partition Information for an Instance

A NICo user can call [`GET /v2/org/{org}/nico/instance/{instance-id}`](api:GET/v2/org/:org/nico/instance/:instanceId) to retrieve information about an Instance. As part of the `200` response body, NICo will return a `nvLinkInterfaces` list that includes both the `nvLinkLogicalPartitionId` and `nvLinkDomainId` for each GPU in the Instance.

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
| ------------- | -------- |
| VPC has `nvLinkLogicalPartitionId`, Instance creation omits `nvLinkInterfaces` | API auto-populates all GPUs to the VPC's NVLink Logical Partition |
| VPC has `nvLinkLogicalPartitionId`, Instance specifies `nvLinkInterfaces` | Instance-level values must align with VPC's Partition, rendering the specification redundant |
| VPC doesn't have `nvLinkLogicalPartitionId` set, Instance specifies `nvLinkInterfaces` | Per-GPU NVLink Logical Partition assignments are used |
| Same `nvLinkLogicalPartitionId` on multiple VPCs | Allowed — no implicit exclusivity |

### How NICo Reconciles NVLink State

NICo runs a periodic reconciler against NMX-C to keep the actual NVLink
partition topology aligned with the desired state implied by tenant instance
configurations. The behaviour matters whenever an operator is
diagnosing latency between an API call and an instance becoming `Ready`.

Each reconciliation pass does the following:

1. Loads every NVLink Logical Partition and every NVLink Physical Partition
   from the NICo database.
1. Resolves the NMX-C endpoint for each chassis and queries its current
   partition list, compute nodes, and GPU membership.
1. Compares observed state against desired state.
1. Issues create / update / remove operations to the fabric-management
   service to converge it onto desired state.
1. Updates per-machine GPU status observations in the NICo database. The
   per-instance `configs_synced.nvlink` field is derived from these
   observations and is what gates the instance's `Ready` state.

Cadence is set by `nvlink_config.monitor_run_interval` (default `60s`).

#### Metrics

The reconciler exposes metrics under the
`carbide_nvlink_partition_monitor_*` namespace. Useful ones:

| Metric | Use | `health` values |
| ------ | --- | --------------- |
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
1. The reconciler observes that GPUs previously assigned to the instance
   are no longer requested in any live partition.
1. The reconciler removes those GPUs from their NMX-C partitions.
1. Once all NVLink state is removed, the machine's GPU status observation
   reflects an empty domain assignment and the host becomes eligible for
   reuse.

When a Logical Partition is deleted, every underlying NVLink Physical
Partition on each NMX-C endpoint backing it is also deleted. The
deletion is rejected if any instance still references the Logical
Partition.

When a host is force-deleted, the instance running on it is implicitly
released and the above cleanup path runs. Operators do not need to detach
NVLink configuration manually before force-deleting.

### Enabling NMX-C-based NVLink Partitioning

NMX-C is the supported gRPC control path for NVLink partition management.

<Note title="Compatibility note">
NMX-M is no longer supported. The legacy
`nico-admin-cli credential add-nmx-m` and `delete-nmx-m` command names remain
available temporarily so existing scripts receive a clear unsupported error;
they do not modify credentials.
</Note>

Configure the NMX-C client under `[nvlink_config]`:

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
nmx_c_endpoint_port        = 9370

allow_insecure = false
```

| Field | Purpose |
| ----- | ------- |
| `nmx_c_tls_ca_cert_path` | Optional PEM containing additional CAs for verifying the NMX-C endpoint's certificate |
| `nmx_c_tls_client_cert_path` | Optional client certificate for mTLS to NMX-C |
| `nmx_c_tls_client_key_path` | Optional client key matching the certificate above |
| `nmx_c_tls_authority` | Optional override for the expected server name during certificate verification (SNI / hostname check) |
| `nmx_c_endpoint_port` | Optional gRPC port used when deriving an endpoint from a switch NVOS IP; defaults to `9370` |
| `allow_insecure` | When `true`, disables TLS verification entirely. Intended for development |

NMX-C endpoints are resolved per chassis. NICo first uses the NVOS IP of a
ready switch with its Fabric Manager control plane configured. If no suitable
switch endpoint is available, NICo falls back to an explicit chassis mapping
stored in the database. Register fallback mappings with `nico-admin-cli`,
keyed by chassis serial:

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

The machine → NMX-C GPU mapping (`nvlink_info`) needs no operator step.
Machines discovered after NVLink partitioning is enabled are populated during
discovery, and the NVLink partition manager populates or repairs the mapping
for any managed machine whose `nvlink_info` is missing or deviates from what
NMX-C reports. `nico-admin-cli machine nvlink-info populate` is retained only
as a deprecated compatibility command and always returns an error; inspect the
stored mapping with `nico-admin-cli machine nvlink-info show <machine-id>`.

### Verifying a Tenant's NVLink Placement

After an instance has been created and the reconciler has had at least one
opportunity to run, an operator can confirm correct placement with the
following checks. There is no single all-in-one health command; the steps
below should be repeatable as a checklist.

1. **Reconciler is running.**
   `carbide_nvlink_partition_monitor_iteration_latency_milliseconds` is being
   recorded. Evaluate the recent increase or rate of
   `carbide_nvlink_partition_monitor_nmxc_connect_error_count` over a time
   window appropriate to the reconcile interval; its historical total alone
   does not indicate a current connection problem.
1. **Logical-partition count matches expectation.**
   `carbide_nvlink_partition_monitor_num_logical_partitions` reflects
   the partitions a site planner expects to exist. A sudden change is
   worth correlating with recent tenant API activity.
1. **Per-instance configuration has converged.** The instance's
   `InstanceStatus` reports `configs_synced.nvlink = true` and the
   `nvLinkInterfaces` list on the instance shows the expected
   `nvLinkLogicalPartitionId` and `nvLinkDomainId` for each GPU.
1. **Per-machine GPU placement.**

   ```bash
   nicocli machine nvlink-info --machine-id <machine-id>
   ```

   Returns the machine's NVLink GPU status observations, including the
   Domain each GPU is currently assigned to. Use this to confirm that
   two instances expected to share an NVLink Logical Partition have
   actually landed in the same NVLink Domain — instances in different
   Domains cannot share GPU memory regardless of having the same
   Logical Partition ID.
1. **Cleanup after release.** After releasing an instance, the same
   `machine nvlink-info` output should show an empty Domain assignment
   on the affected GPUs within one or two reconcile intervals. Failure
   to clear indicates the reconciler could not remove the GPU from its NMX-C
   partition; investigate the corresponding connect-error and op-latency
   metrics.
