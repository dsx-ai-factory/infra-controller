# Networking Integrations

NVIDIA Infra Controller (NICo) integrates with various network virtualization solutions that allow the bare metal instances of tenants to communicate on isolated partitions. Any instances that are not part of the same partition are not able to participate in communication - irrespective of whether these instances are owned by the same tenant or a different tenant.

Networking integrations in NICo achieve this through the following patterns:

## Workflows

### Tenant partition management

1. Tenants have APIs for managing a set of network partitions for their instances. Examples of these partitions are
   - VPCs (for ethernet)
   - InfiniBand partitions
   - NVLink logical partitions
2. There might be additional sub-apis for more in-depth management of these partitions, e.g. if resources (like IPs) need to be dynamically added to the partition.
3. Tenants can query for the status of the partition via APIs. Each partition has a lifecycle status (Provisioning, Ready, Terminating).
4. Partitions can only be fully deleted once there are no more instances associated with them. State machines for these objects with checks for the terminating state assure that.
5. admin tools (web-ui and admin-cli) make site admins aware of these resources and their state

### Tenant instance interface configurations

1. Tenants are able to associate the network interfaces of their instances with a partition they created upfront. This configuration can either happen at instance creation time, or at a later time using `UpdateInstanceConfig` calls.
2. In order to support Virtual Machines on top of instances, partitions should be configurable on a per-interface base instead of per-host base. This allows the VM system to attach different interfaces (PCI PFs) to different VMs.
3. When the instance is updated, the tenant will get accurate status if networking on the machine has been reconfigured to use the new partition using `configs_synced` attributes that are part of the instance status. This flag will also influence the overall readiness of the instance that is shown in the `state` field: If networking is not fully configured, the instance will show a status of `Configuring`. Once networking is configured, it will move to `Ready`.
4. When the instance configuration is updated, the `config_version` field that is part of the `Instance` will get incremented.
5. On initial provisioning, the state machine will block booting into the tenant OS until the desired configuration is achieved. This guarantees that once the instance is booted, it can immediately communicate with all other instances of the tenant that share the partition.
6. On instance termination, the termination flow blocks on the until the networking interfaces are reconfigured to no longer be part of any partition (the instance is isolated on the network). That assures that once the tenant is notified that the instance is deleted, it is at least fully isolated and can no longer show up as a "ghost instance" - even in case the disk might not be cleaned up yet. The "desired" instance configuration that is submitted by the tenant and reflected in the `InstanceConfig` message will not change during that workflow. This means the system must also take another field in the machine object into account to switch from "tenant desired networking" to "isolated network".

### Machine Capabilities and Instance types

- Tenants need to know how they can actually configure their instances. Valid configurations depend on the hardware. E.g. in an instance with 4 connected InfiniBand ports, tenants can associate each of these ports with a separate partition. However tenants are not able to configure instances without InfiniBand ports for IB.
- Tenants learn about the support configurations via "Instance Types", which hold a list of capabilities. Each type of networking capability informs a tenant on how the respective interface can be configured. This means for each configurable interface, the instance type should list a respective capability.
- The set of capabilities encoded in instance types must match or be a subset of the capabilities associated with a `Machine`. Machine capabilities are detected during the hardware discovery and ingestion phases. They are viewable by site administrators via debug tools.
  - During Machine ingestion, data about all network interfaces is collected both in-band (using scout) and out-of-band (using site-explorer). The data is stored within `machine` and `machine_topologies` tables
  - Based on the raw discovery data, "machine capabilities" (type `MachineCapabilitiesSet`) are computed by the core service and presented to site administrators. These capabilities inform users about the amount of interfaces which are configurable. For each network integration, a new type of machine capability is required. For example, InfiniBand uses the `MachineCapabilityAttributesInfiniband` capability, while nvlink uses the `MachineCapabilityAttributesGpu` capability.
- The SKU validation feature can include checks whether any newly ingested host includes the expected amount of network interfaces - where each network interface is typically described as a machine capability.

## Implementation requirements and considerations

To implement these workflows, the following patterns had been developed and proven successful in NICo:

### Desired state vs actual state of network interfaces

- For each network interface on each machine, NICo tracks both the desired state (target network partition and other configs) as well as the actual state.
- The desired state is a combination of the "tenant requested state" as well as a set of configurations internally managed by NICo.
  - The tenant requested state is stored fully in the `InstanceConfig` object
  - The internal requested state is stored in the `ManagedHostNetworkConfig` that is part of the `machine` table in the database. The most important field here is the `use_admin_network` field which controls whether tenant configurations are overridden and that the machine should indeed be placed onto an isolated/admin network.
- The actual state is stored as part of the `Machine` database object. The integration between NICo and the respective networking subsystem is responsible for updating it there. All other workflows within NICo will use this observed state for decision making instead of reaching out to any external services. This internal caching of observed state keeps workflows deterministic and reliable, since they act on the same source of truth. It also helps with reactivity and scaling, since all other code paths do not need to reach out to an external service to learn about network state.

  Two integration patterns have been developed over time:
    1. The actual observed state is updated by a "monitoring and reconciliation task" specific to the networking technology. Examples of this integration are the `IbFabricMonitor` services (for InfiniBand) and `NvlPartitionMonitor` (for NVLink). This kind of monitoring and integration is favorable if the external networking is controlled via an external service, since the integration is able to fetch the actual networking state for more than one device and host at the same time and can update all affected machine objects at once.
    2. The actual observed state is updated for each interface or host by a service associated with this interface by making an API call into NICo. An example of this integration is `dpu-agent` sending the observed DPU configuration via a gRPC call (`RecordDpuNetworkStatus`).
- Site admins need to be able to view both the desired configuration for any interface as well as the actual configuration.

### State Reconciliation

There needs to be a mechanism that periodically compares the expected networking configuration with the desired networking configuration. If they are not in-sync, the respective components needs to take all the required actions to bring the configurations in sync.

1. For networking technologies where an external service is used to control partitioning (NVLink, InfiniBand), the `Monitor` background tasks are used to achieve this goal. If they detect a configuration mismatch, they perform API calls to the external networking service to resolve the problem.
2. For other integrations, an external agent can pull the desired configuration for any host, perform (potentially local) configuration changes, before reporting the new state back to NICo. This approach is taken for DPUs.

### Instance lifecycle and "tenant feedback"

1. The `InstanceStatus` should define a `configs_synced` field that shows whether the network configuration for all interfaces of the instance is applied. There should be a `configs_synced` field per network integration (e.g. `InstanceStatus::infiniband::configs_synced`) in addition to the overall `configs_synced` value.
    - The value of the per-technology `configs_synced` fields should be derived by comparing the desired network configurations to the actual configuration as stored in the `Machine` object. This is implemented within `InstanceStatus::from_config_and_observation`.
    - The value of the aggregate `configs_synced` field is the logical **and** of all individual `configs_synced` fields in the `InstanceStatus` message.
2. The instances tenant status (as communicated via `Instance::status::tenant::state`) should take into account whether the desired configuration is applied:
    - If an instance is still in one of the provisioning states (anything before `Ready`), it will show a tenant status of `Provisioning`.
    - If the instance ever had been `Ready`, and the actual network configuration deviates from the intended configuration, the status should show `Configuring`.
    - If instance termination has been requested, the instances status should show `Terminating` independent of network configurations.
3. The instance state machine should have guards in certain states that wait until the desired network configurations are applied:
    - During initial provisioning, `WaitingForNetworkConfig` waits for current
      DPU observations and matching configuration versions. It also waits for
      health alerts that prevent host state changes and the primary p0 PXE gate.
      Refer to
      [DPU ToR Uplink Health](../dpu-management/dpu_configuration.md#dpu-tor-uplink-health)
      for the BGP policy.
    - Immediately before a normal PXE restart, `WaitingForRebootToReady`
      repeats the aggregate health and primary p0 checks. Instance deletion and
      explicit custom iPXE requests bypass these checks.
    - A live `UpdateInstanceConfig` request uses
      `NetworkConfigUpdate/WaitingForConfigSynced`. This state waits for current
      DPU observations and health alerts that prevent host state changes. It does
      not apply the primary p0 PXE gate or restart the host.
    - During instance termination, one state in the state machine should wait until the machine is isolated from any other machine in the network. If this step is omitted (to let the machine proceed termination in the case of an unhealthy network fabric), the respective machine must at least be tagged with a health alert that would prevent a different tenant from using the host. Both options guarantee that no other tenant will get access to the tenants network partition.

### Machine Capabilities and Instance types

1. The machine capabilities definitions need to be extended for each new networking technology.
2. Hardware enumeration processes need to be updated in order to fetch and store the new types of capabilities.

### Fabric health monitoring and debug capabilities

1. If a network subsystem is managed via an external fabric monitor service, the health of the service (as visible to NICo) should be monitored, in order to allow NICo admins to understand whether there are upstream issues that would lead to network configurations not being applied. Common metrics that should be monitored are upstream service availability (request success rates) as well as latencies for any API calls.
2. For certain networking technologies, NICo integrated debug tools that allow NICo operators to view the state of the fabric manager service without requiring credentials. The UFM explorer functionality in NICo is an example of such a tool. For any future integration, similar tools should get integrated if possible.

## Configurability

- Whether a certain network virtualization technology is available in a NICo deployment should be configurable via NICo config files.

## Managed Host force delete support

- When a host is force-deleted from the system, it will not go through the regular deprovisioning states. This means without extra support, networking configurations for the host would still persist in external agents and fabric managers.
- To prevent that, the force-delete code-path should contain extra logic to detach the host from partitions via external fabric manager APIs.

## External fabric manager client libraries

- If an external fabric manager is used to observe interface state and set configuration, a client library in Rust is required.
- Interactions with external fabric managers will require credentials. These should be read from the file system, and get injected via an external service (e.g. K8S secrets).
