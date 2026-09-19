# Multi-Node Maintenance Executor (MNME)

## Software Design Document

## Revision History

| Version | Date | Modified By | Description |
| :---: | :---: | :---- | :---- |
| 0.1 | 06/25/2026 | Matthias Einwag | Initial version |
| 0.2 | 08/07/2026 | Matthias Einwag | Add APIs for listing and cancelling maintenance operations |
| 0.3 | 08/07/2026 | Matthias Einwag | Correct spelling and grammar |
| 0.4 | 08/07/2026 | Matthias Einwag | Define independent maintenance safety overrides |

## Overview

This document introduces a new component within NICo-core that allows multi-node maintenance operations to be executed in a conflict-free and efficient manner.

The Multi-Node Maintenance Executor (MNME) replaces the current maintenance execution through the rack state machine. It improves upon it in the following areas:

- MNME allows users to apply multiple maintenance operations to different trays within a rack concurrently, provided that it determines there are no side effects between the operations.
- MNME allows users to express safe maintenance operations for systems where maintenance side effects do not exactly match rack boundaries. It supports architectures where a rack is no longer equivalent to a scale-up domain. This includes systems where:
  - the maintenance domain (e.g. an NVLink failure domain) is smaller than a rack, such as when a GB200 rack is subdivided into multiple "mini racks"
  - the maintenance domain spans multiple racks due to shared resources of any kind

Implementing MNME moves most logic out of the rack state machine into a separate component, reducing the need to expose a type in NICo (`message Rack`) for something that does not physically exist.

### What are multi-node maintenance operations?

Multi-node maintenance operations are maintenance operations that either directly or indirectly affect more than one node. Here, a node refers to any concrete hardware component that NICo manages:

- Compute trays
- Switches
- Power shelves

A firmware update on five CPU trays is a multi-node maintenance operation that **directly** affects multiple nodes: all five trays are unavailable for their regular workloads while the update is applied.

A reconfiguration of an NVLink domain is a multi-node maintenance operation that **indirectly** affects multiple nodes: although it might directly change only a single switch tray, all attached compute trays in the data center might experience a service disruption.

## MNME - User experience and APIs

NICo site administrators use MNME-related APIs in a similar fashion to the current rack-scale maintenance APIs:

```proto
rpc OnDemandRackMaintenance(RackMaintenanceOnDemandRequest) returns (RackMaintenanceOnDemandResponse);

message RackMaintenanceOnDemandRequest {
  common.RackId rack_id = 1;
  RackMaintenanceScope scope = 2;
}

message RackMaintenanceScope {
  repeated string machine_ids = 1;
  repeated string switch_ids = 2;
  repeated string power_shelf_ids = 3;
  // Which maintenance activities to run. Empty means all activities.
  repeated MaintenanceActivityConfig activities = 4;
}

message MaintenanceActivityConfig {
  oneof activity {
    FirmwareUpgradeActivity firmware_upgrade = 1;
    ConfigureNmxClusterActivity configure_nmx_cluster = 2;
    PowerSequenceActivity power_sequence = 3;
    NvosUpdateActivity nvos_update = 4;
  }
}
```

There are, however, several key differences:

1. The user no longer specifies a `rack_id`, but only the set of nodes on which to perform operations. The system automatically detects relationships between the affected nodes and other nodes managed by NICo and ensures that all nodes are in a "safe" state that allows the changes to be performed without affecting service. For example:
   1. If an NVSwitch update is triggered, MNME automatically detects all compute trays associated with the same NVLink domain as the switch and ensures that they are in a safeguarded state (explained below) before applying the update. The compute tray nodes do not need to be referenced explicitly.
   2. If an update is scheduled for a set of compute trays in the rack (e.g. MachineA and MachineB) while another update on other machines is in progress (e.g. MachineC), the new update is executed immediately. If the sets of machines overlap, however, the new update is scheduled behind the existing one.
2. NICo site admins can explicitly reference additional nodes affected by the maintenance operation for use cases where the system cannot determine the full impact. Every referenced node is also moved into a safeguarded state.
3. NICo site admins can independently skip conflict exclusion, skip moving nodes into a safeguarded state, or skip both protections. These overrides are intended for advanced use cases where a system administrator or external system has determined that the corresponding protections are unnecessary. Impacted-node discovery and health validation are never skipped by these overrides.
4. Site admins no longer observe update progress through status changes on the rack state machine. Instead, they query the maintenance status using the ID returned by the maintenance request.
5. Site admins can cancel a scheduled maintenance operation by using the ID returned by the maintenance request. An operation cannot be cancelled after it starts.
6. Site admins can retrieve the IDs of all maintenance operations, optionally filtered by lifecycle state, and then retrieve their statuses by ID.

### Proposed API shape

All MNME APIs are restricted to site administrators.

```proto
// Creates a multi-node maintenance operation and returns the ID used to query
// or cancel it.
rpc ScheduleMultiNodeMaintenance(MultiNodeMaintenanceRequest) returns (MultiNodeMaintenanceResponse);

// Returns all maintenance operation IDs when the filter is empty. If state is
// provided, only operations in that lifecycle state are returned.
rpc FindMultiNodeMaintenanceOperationIds(MultiNodeMaintenanceOperationSearchFilter) returns (MultiNodeMaintenanceOperationIdList);

// Returns the status and available results for the requested maintenance
// operation IDs.
rpc FindMultiNodeMaintenanceOperationsByIds(MultiNodeMaintenanceOperationsByIdsRequest) returns (MultiNodeMaintenanceOperationList);

// Transitions an operation from `Scheduled` to the terminal `Cancelled` state
// and returns its updated status. Cancellation is idempotent: cancelling an
// operation that is already `Cancelled` succeeds and returns its current
// status. The API returns `NOT_FOUND` if the maintenance ID does not exist and
// `FAILED_PRECONDITION` if the operation is already `In Progress` or has
// completed in any other terminal state. In particular, cancellation does not
// stop an activity that has started and does not attempt to roll back hardware
// changes. The transition from `Scheduled` to either `Cancelled` or
// `In Progress` must be atomic. If cancellation wins the race, the executor
// must not start the operation. If execution wins the race, cancellation fails
// with `FAILED_PRECONDITION`. Cancelled operations remain queryable through
// `FindMultiNodeMaintenanceOperationsByIds`.
rpc CancelMultiNodeMaintenance(CancelMultiNodeMaintenanceRequest) returns (MultiNodeMaintenanceStatus);

message MultiNodeMaintenanceRequest {
  // The set of nodes on which the operation will be carried out directly.
  // The system automatically determines additional nodes affected by the change.
  MaintenanceScope scope = 1;

  // Which maintenance activities to run. Empty means all activities.
  repeated MaintenanceActivityConfig activities = 2;

  // Additional nodes that are considered impacted for conflict exclusion and
  // moved into a safeguarded state during maintenance by default.
  MaintenanceScope extra_safeguard_nodes = 3;

  // Optional dangerous overrides. If omitted, or if both flags are false, all
  // safety protections remain enabled.
  DangerousMaintenanceOverrides danger_overrides = 4;

  // By default, the system starts updates only when all target nodes are
  // healthy. Adding health alert classifications to this list permits update
  // scheduling on nodes that are unhealthy for the specified reasons.
  repeated string ignored_health_alert_classifications = 5;
}

message DangerousMaintenanceOverrides {
  // Skips waiting for conflicting maintenance operations and acquiring
  // exclusive node reservations. Impacted-node discovery still occurs, and
  // the operation remains visible to conflict checks performed by operations
  // that do not enable this override.
  bool skip_conflict_exclusion = 1;

  // Skips moving impacted nodes into states that guarantee they are not being
  // used by tenants or workloads.
  bool skip_node_safeguarding = 2;

  // Required when either override is enabled. The reason is persisted with
  // the operation and included in its audit events.
  string reason = 3;
}

message MultiNodeMaintenanceResponse {
  // An ID that can be used to query the status of the maintenance operation.
  common.UUID maintenance_id = 1;
}

message CancelMultiNodeMaintenanceRequest {
  // The ID returned by `ScheduleMultiNodeMaintenance`.
  common.UUID maintenance_id = 1;
}

message MultiNodeMaintenanceOperationSearchFilter {
  // Exact value of MultiNodeMaintenanceStatus.status.state to match.
  // If omitted, operations in all lifecycle states are returned.
  optional string state = 1;
}

message MultiNodeMaintenanceOperationIdList {
  repeated common.UUID maintenance_operation_ids = 1;
}

message MultiNodeMaintenanceOperationsByIdsRequest {
  repeated common.UUID maintenance_operation_ids = 1;
}

// Contains the set of nodes affected by a maintenance operation.
message MaintenanceScope {
  repeated string machine_ids = 1;
  repeated string switch_ids = 2;
  repeated string power_shelf_ids = 3;
  // If specified, targets all trays within each rack.
  repeated RackId racks = 4;
}

message MaintenanceActivityConfig {
  oneof activity {
    FirmwareUpgradeActivity firmware_upgrade = 1;
    ConfigureNmxClusterActivity configure_nmx_cluster = 2;
    PowerSequenceActivity power_sequence = 3;
    NvosUpdateActivity nvos_update = 4;
  }
}

message MultiNodeMaintenanceOperationList {
  repeated MultiNodeMaintenanceStatus operations = 1;
}

message MultiNodeMaintenanceStatus {
  common.UUID id = 1;
  // Current state of the operation.
  LifecycleStatus status = 2;
  // Results become available as individual parts of the operation finish.
  repeated MultiNodeMaintenanceResult results = 3;
}
```

### What is a "safeguarded state"?

A safeguarded state guarantees that the node is not used by a NICo tenant. For compute trays, the safeguarded state is usually one in which the scout image is booted. In this state, the tenant does not own the tray.

Conflict exclusion is a separate protection that prevents concurrent maintenance operations from affecting the same node. Separating these protections allows callers to bypass either one independently.

#### How is safeguarding implemented for compute trays?

Safeguarding for compute-trays follows the existing model for DPU firmware updates and in-band firmware updates:

- Once the update execution stars, a certain `updated_requested` or `safeguarding_request` is stored on the Machine
- If the Machine is used as an instance, this will lead to an "update requested" flag being set on the Instance: `instance.status.update` will signal the tenant that restarting the instance for an update is required.
- The host would change state into a state which waits for the reboot (and safeguarding) to be applied, e.g. `ASSIGNED/WAITINGFORUPDATEREBOOT`.
- The tenant needs to reboot their instance with the flag `apply_updates_on_reboot` set to `true`. This mechanism should get extended for multi-node updates so that the tenant only has a certain amount of time to initiate the update. Once the grace time elapses, the reboot would be enforced. Otherwise a single tenant not opting into the update would make the hosts of all other tenants on the rack or domain unavailable for an extended time.
- If the tenant reboots the Instance with the flag set, the host boots the scout OS and thereby makes the instance unavailable for the tenant. The tenant observes an instance state of `UPDATING`.
- The host state machine observes the reboot and scout OS starting and moves the host into the safeguard state (e.g. `ASSIGNED/APPLYINGUPDATES`)
- Once the host is in this state, the MNME update execution engine can aply the updates.

### Safety overrides

Conflict exclusion and node safeguarding are independent protections. Their combinations have the following behavior:

| `skip_conflict_exclusion` | `skip_node_safeguarding` | Behavior |
| :---: | :---: | :--- |
| `false` | `false` | Normal safe execution. Conflicting operations are serialized, and impacted nodes are moved out of tenant or workload use. |
| `true` | `false` | Conflicting operations may run concurrently, but impacted nodes are still moved out of tenant or workload use. |
| `false` | `true` | Conflicting operations remain serialized, but impacted nodes are not moved out of tenant or workload use. |
| `true` | `true` | Both protections are bypassed. |

Neither override changes impacted-node discovery, health validation, or status and result persistence. Health validation continues to honor `ignored_health_alert_classifications`.

## MNME implementation

MNME is a new, independent subcomponent within NICo-core. It is scheduled independently as a periodic task, like SiteExplorer, NVLink Manager, and other components.

In each iteration of the periodic task, MNME will perform the following steps:

1. Identify executable maintenance operations and start them via the following steps:
   1. Query the list of maintenance operations in the `Scheduled` state. These operations must be stored in the NICo database after a `ScheduleMultiNodeMaintenance` call. Operations in the `Cancelled` state are terminal and must not be considered for execution.
   2. For each planned maintenance operation, determine the full set of impacted nodes. This always occurs, regardless of the configured safety overrides. The set is the union of directly impacted nodes, automatically derived indirectly impacted nodes, and `extra_safeguard_nodes`. Indirectly impacted nodes can be determined based on the activity type and links between entities. For example:
      - Pure compute tray firmware updates do not directly affect other nodes, so the list of indirectly impacted nodes is empty.
      - Switch operations affect all nodes that share the same NVLink domain.
   3. Validate the health of all impacted nodes. Health validation always occurs, but alerts whose classifications appear in `ignored_health_alert_classifications` do not prevent execution.
   4. Unless `skip_conflict_exclusion` is enabled, check whether the impacted-node set intersects with that of any operation already in progress. If a conflict exists, leave the operation in the `Scheduled` state. This check could be performed in two ways:
      1. Directly load information about all in-progress operations and perform an intersection check.
      2. Check health alerts on affected nodes (`MultiNodeUpdateInProgress`). This requires operation startup to place the health alert automatically on all affected nodes.
   5. Atomically advance the operation from `Scheduled` to `In Progress` and publish its impacted-node set for subsequent conflict checks. A conditional state transition prevents an operation that was cancelled concurrently from starting. If conflict exclusion is enabled, acquiring the exclusive node reservations and advancing the operation must be a single atomic action. If `skip_conflict_exclusion` is enabled, the operation may advance despite existing conflicts and does not acquire exclusive reservations.
2. For all updates in the `In Progress` state, perform the following steps:
   1. Unless `skip_node_safeguarding` is enabled, initiate triggers that move the nodes into a safeguarded state. For example, for compute trays, set a flag that makes the nodes boot the scout image on the next restart.
   2. If safeguarding was not skipped, wait for the impacted nodes to reach the safeguarded state.
   3. Apply the update. This can happen, for example, by calling RMS APIs and waiting for the results.
   4. After the update is complete and unless `skip_node_safeguarding` is enabled, set a flag that releases the nodes from the safeguarded state in their individual state machines.
   5. Wait for all released nodes to finish exiting the safeguarded state.

The steps in item 2 are equivalent to those executed by the rack state machine for on-demand maintenance.

### Implementation options

As described in the previous section, MNME contains two major components: a deployment-wide process that identifies newly executable maintenance operations, followed by the execution of all in-progress operations, which can be parallelized.

These two steps can be implemented in the following ways:

- A single task performs both steps 1 and 2. Step 2 could be parallelized through a fork/join mechanism using `tokio::spawn` or `tokio::task::JoinSet`. This model would be equivalent to components such as NVLink Monitor.
- The periodic MNME main task performs only step 1. To schedule updates, it creates `MultiNodeMaintenance` objects whose lifecycles are managed by the existing state controller framework. This model would allow the reuse of code for concurrent state management across a set of maintenance objects.

## Supported maintenance activities

MNME would initially support all maintenance activities that are currently available on rack-level APIs, including out-of-band firmware updates using RMS and NVLink setup and configuration.

It should from there be extended to support all other maintenance activities available for NICo-managed components in-band software updates that are currently flowing through another set of APIs and code path.

DPU updates and maintenance could be supported by the same framework, but further investigation is required to determine the complexity of extracting them from the ManagedHost state machines.

## Out of scope

MNME's primary scope is to identify which maintenance operations are safe to execute at any point in time and to start them. It enforces synchronization with individual node state machines before triggering the actual update.

It is therefore a building block rather than a fully featured fleet management system.

- MNME will not decide which updates to install at any point in time. This is left to external callers of MNME APIs.
- MNME cannot schedule maintenance further in the future ("next Monday at 2 a.m.").
- MNME cannot determine whether executing an update would violate minimum fleet health SLAs. Any update scheduling based on fleet health must be initiated by an external system.

The MNME APIs are expected to be used by an external maintenance management system according to the fleet health requirements of the affected deployment.
