# Protocol Documentation
<a name="top"></a>

## Table of Contents

- [flow.proto](#flow-proto)
    - [AddComponentRequest](#v1-AddComponentRequest)
    - [AddComponentResponse](#v1-AddComponentResponse)
    - [AddTaskScheduleScopeRequest](#v1-AddTaskScheduleScopeRequest)
    - [AddTaskScheduleScopeResponse](#v1-AddTaskScheduleScopeResponse)
    - [AssociateRuleWithRackRequest](#v1-AssociateRuleWithRackRequest)
    - [AttachRacksToNVLDomainRequest](#v1-AttachRacksToNVLDomainRequest)
    - [BMCInfo](#v1-BMCInfo)
    - [BringUpRackRequest](#v1-BringUpRackRequest)
    - [BuildInfo](#v1-BuildInfo)
    - [CancelTaskRequest](#v1-CancelTaskRequest)
    - [CancelTaskResponse](#v1-CancelTaskResponse)
    - [CheckScheduleConflictsRequest](#v1-CheckScheduleConflictsRequest)
    - [CheckScheduleConflictsResponse](#v1-CheckScheduleConflictsResponse)
    - [Component](#v1-Component)
    - [ComponentDiff](#v1-ComponentDiff)
    - [ComponentOperationStatus](#v1-ComponentOperationStatus)
    - [ComponentTarget](#v1-ComponentTarget)
    - [ComponentTargets](#v1-ComponentTargets)
    - [ComponentTypes](#v1-ComponentTypes)
    - [CreateExpectedRackRequest](#v1-CreateExpectedRackRequest)
    - [CreateExpectedRackResponse](#v1-CreateExpectedRackResponse)
    - [CreateNVLDomainRequest](#v1-CreateNVLDomainRequest)
    - [CreateNVLDomainResponse](#v1-CreateNVLDomainResponse)
    - [CreateOperationRuleRequest](#v1-CreateOperationRuleRequest)
    - [CreateOperationRuleResponse](#v1-CreateOperationRuleResponse)
    - [CreateTaskScheduleRequest](#v1-CreateTaskScheduleRequest)
    - [DeleteComponentRequest](#v1-DeleteComponentRequest)
    - [DeleteComponentResponse](#v1-DeleteComponentResponse)
    - [DeleteOperationRuleRequest](#v1-DeleteOperationRuleRequest)
    - [DeleteRackRequest](#v1-DeleteRackRequest)
    - [DeleteRackResponse](#v1-DeleteRackResponse)
    - [DeleteTaskScheduleRequest](#v1-DeleteTaskScheduleRequest)
    - [DetachRacksFromNVLDomainRequest](#v1-DetachRacksFromNVLDomainRequest)
    - [DeviceInfo](#v1-DeviceInfo)
    - [DeviceSerialInfo](#v1-DeviceSerialInfo)
    - [DisassociateRuleFromRackRequest](#v1-DisassociateRuleFromRackRequest)
    - [ExternalRef](#v1-ExternalRef)
    - [FieldDiff](#v1-FieldDiff)
    - [Filter](#v1-Filter)
    - [GetComponentInfoByIDRequest](#v1-GetComponentInfoByIDRequest)
    - [GetComponentInfoBySerialRequest](#v1-GetComponentInfoBySerialRequest)
    - [GetComponentInfoResponse](#v1-GetComponentInfoResponse)
    - [GetComponentsRequest](#v1-GetComponentsRequest)
    - [GetComponentsResponse](#v1-GetComponentsResponse)
    - [GetListOfNVLDomainsRequest](#v1-GetListOfNVLDomainsRequest)
    - [GetListOfNVLDomainsResponse](#v1-GetListOfNVLDomainsResponse)
    - [GetListOfRacksRequest](#v1-GetListOfRacksRequest)
    - [GetListOfRacksResponse](#v1-GetListOfRacksResponse)
    - [GetOperationRuleRequest](#v1-GetOperationRuleRequest)
    - [GetRackInfoByIDRequest](#v1-GetRackInfoByIDRequest)
    - [GetRackInfoBySerialRequest](#v1-GetRackInfoBySerialRequest)
    - [GetRackInfoResponse](#v1-GetRackInfoResponse)
    - [GetRackRuleAssociationRequest](#v1-GetRackRuleAssociationRequest)
    - [GetRackRuleAssociationResponse](#v1-GetRackRuleAssociationResponse)
    - [GetRacksForNVLDomainRequest](#v1-GetRacksForNVLDomainRequest)
    - [GetRacksForNVLDomainResponse](#v1-GetRacksForNVLDomainResponse)
    - [GetTaskScheduleRequest](#v1-GetTaskScheduleRequest)
    - [GetTasksByIDsRequest](#v1-GetTasksByIDsRequest)
    - [GetTasksByIDsResponse](#v1-GetTasksByIDsResponse)
    - [Identifier](#v1-Identifier)
    - [IngestRackRequest](#v1-IngestRackRequest)
    - [ListOperationRulesRequest](#v1-ListOperationRulesRequest)
    - [ListOperationRulesResponse](#v1-ListOperationRulesResponse)
    - [ListRackRuleAssociationsRequest](#v1-ListRackRuleAssociationsRequest)
    - [ListRackRuleAssociationsResponse](#v1-ListRackRuleAssociationsResponse)
    - [ListTaskScheduleScopesRequest](#v1-ListTaskScheduleScopesRequest)
    - [ListTaskScheduleScopesResponse](#v1-ListTaskScheduleScopesResponse)
    - [ListTaskSchedulesRequest](#v1-ListTaskSchedulesRequest)
    - [ListTaskSchedulesResponse](#v1-ListTaskSchedulesResponse)
    - [ListTasksRequest](#v1-ListTasksRequest)
    - [ListTasksResponse](#v1-ListTasksResponse)
    - [Location](#v1-Location)
    - [NVLDomain](#v1-NVLDomain)
    - [OperationRule](#v1-OperationRule)
    - [OperationTargetSpec](#v1-OperationTargetSpec)
    - [OrderBy](#v1-OrderBy)
    - [Pagination](#v1-Pagination)
    - [PatchComponentRequest](#v1-PatchComponentRequest)
    - [PatchComponentResponse](#v1-PatchComponentResponse)
    - [PatchRackRequest](#v1-PatchRackRequest)
    - [PatchRackResponse](#v1-PatchRackResponse)
    - [PauseTaskScheduleRequest](#v1-PauseTaskScheduleRequest)
    - [PowerOffRackRequest](#v1-PowerOffRackRequest)
    - [PowerOnRackRequest](#v1-PowerOnRackRequest)
    - [PowerResetRackRequest](#v1-PowerResetRackRequest)
    - [PurgeComponentRequest](#v1-PurgeComponentRequest)
    - [PurgeComponentResponse](#v1-PurgeComponentResponse)
    - [PurgeRackRequest](#v1-PurgeRackRequest)
    - [PurgeRackResponse](#v1-PurgeRackResponse)
    - [QueueOptions](#v1-QueueOptions)
    - [Rack](#v1-Rack)
    - [RackPosition](#v1-RackPosition)
    - [RackRuleAssociation](#v1-RackRuleAssociation)
    - [RackTarget](#v1-RackTarget)
    - [RackTargets](#v1-RackTargets)
    - [RemoveTaskScheduleScopeRequest](#v1-RemoveTaskScheduleScopeRequest)
    - [ResumeTaskScheduleRequest](#v1-ResumeTaskScheduleRequest)
    - [ScheduleConfig](#v1-ScheduleConfig)
    - [ScheduleSpec](#v1-ScheduleSpec)
    - [ScheduledOperation](#v1-ScheduledOperation)
    - [SetRuleAsDefaultRequest](#v1-SetRuleAsDefaultRequest)
    - [StringQueryInfo](#v1-StringQueryInfo)
    - [SubmitTaskResponse](#v1-SubmitTaskResponse)
    - [Task](#v1-Task)
    - [TaskSchedule](#v1-TaskSchedule)
    - [TaskScheduleScope](#v1-TaskScheduleScope)
    - [TriggerTaskScheduleRequest](#v1-TriggerTaskScheduleRequest)
    - [UUID](#v1-UUID)
    - [UpdateOperationRuleRequest](#v1-UpdateOperationRuleRequest)
    - [UpdateTaskScheduleRequest](#v1-UpdateTaskScheduleRequest)
    - [UpdateTaskScheduleScopeRequest](#v1-UpdateTaskScheduleScopeRequest)
    - [UpdateTaskScheduleScopeResponse](#v1-UpdateTaskScheduleScopeResponse)
    - [UpgradeFirmwareRequest](#v1-UpgradeFirmwareRequest)
    - [ValidateComponentsRequest](#v1-ValidateComponentsRequest)
    - [ValidateComponentsResponse](#v1-ValidateComponentsResponse)
    - [VersionRequest](#v1-VersionRequest)
  
    - [BMCType](#v1-BMCType)
    - [ComponentFilterField](#v1-ComponentFilterField)
    - [ComponentOrderByField](#v1-ComponentOrderByField)
    - [ComponentType](#v1-ComponentType)
    - [ConflictStrategy](#v1-ConflictStrategy)
    - [DiffType](#v1-DiffType)
    - [LeakStatus](#v1-LeakStatus)
    - [OperationType](#v1-OperationType)
    - [OverlapPolicy](#v1-OverlapPolicy)
    - [Phase](#v1-Phase)
    - [PowerControlOp](#v1-PowerControlOp)
    - [RackFilterField](#v1-RackFilterField)
    - [RackOrderByField](#v1-RackOrderByField)
    - [ScheduleSpecType](#v1-ScheduleSpecType)
    - [TaskExecutorType](#v1-TaskExecutorType)
    - [TaskStatus](#v1-TaskStatus)
  
    - [Flow](#v1-Flow)
  
- [Scalar Value Types](#scalar-value-types)



<a name="flow-proto"></a>
<p align="right"><a href="#top">Top</a></p>

## flow.proto



<a name="v1-AddComponentRequest"></a>

### AddComponentRequest
AddComponent - ingest a single component into the inventory. The component
may optionally be attached to an existing rack via component.rack_id; when
rack_id is omitted the component is stored without a rack assignment and
can be moved into a rack later via PatchComponent.


| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| component | [Component](#v1-Component) |  | Required: the component to add. component.rack_id is optional. |






<a name="v1-AddComponentResponse"></a>

### AddComponentResponse



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| component | [Component](#v1-Component) |  | The created component |






<a name="v1-AddTaskScheduleScopeRequest"></a>

### AddTaskScheduleScopeRequest
AddTaskScheduleScopeRequest adds one or more scope entries to a schedule.
Supports rack-level targeting (with optional component-type filter) and
component-level targeting (specific components by UUID or external reference).
For component-level targets the server resolves which rack each component
belongs to and groups them into per-rack scope entries automatically.
Racks already present in the scope have their component filter merged with the
incoming filter rather than erroring; racks not yet present are added.
Existing racks are never removed — use UpdateTaskScheduleScope for replace semantics.


| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| schedule_id | [UUID](#v1-UUID) |  |  |
| target_spec | [OperationTargetSpec](#v1-OperationTargetSpec) |  |  |






<a name="v1-AddTaskScheduleScopeResponse"></a>

### AddTaskScheduleScopeResponse
AddTaskScheduleScopeResponse returns the newly created scope entries.


| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| scopes | [TaskScheduleScope](#v1-TaskScheduleScope) | repeated |  |






<a name="v1-AssociateRuleWithRackRequest"></a>

### AssociateRuleWithRackRequest



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| rack_id | [UUID](#v1-UUID) |  |  |
| rule_id | [UUID](#v1-UUID) |  |  |






<a name="v1-AttachRacksToNVLDomainRequest"></a>

### AttachRacksToNVLDomainRequest



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| nvl_domain_identifier | [Identifier](#v1-Identifier) |  |  |
| rack_identifiers | [Identifier](#v1-Identifier) | repeated |  |






<a name="v1-BMCInfo"></a>

### BMCInfo



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| type | [BMCType](#v1-BMCType) |  |  |
| mac_address | [string](#string) |  |  |
| ip_address | [string](#string) | optional |  |






<a name="v1-BringUpRackRequest"></a>

### BringUpRackRequest



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| target_spec | [OperationTargetSpec](#v1-OperationTargetSpec) |  | Target racks for bring-up |
| description | [string](#string) |  | optional task description |
| rule_id | [UUID](#v1-UUID) | optional | optional: override rule resolution with a specific rule |
| override_readiness_check | [bool](#bool) |  | When true, allow the bring-up sequence (which may power-cycle hosts and reset rack-scoped components) to proceed even if any host in scope is reported as not ready for the operation by its persisted ComponentOperationStatus. Intended for operator-supervised maintenance where tenant impact has been acknowledged out-of-band; the bypass is recorded in the server log. |






<a name="v1-BuildInfo"></a>

### BuildInfo



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| version | [string](#string) |  | e.g., v2025.11.19 |
| build_time | [string](#string) |  | e.g., 2025-01-27T10:30:00Z |
| git_commit | [string](#string) |  | e.g., abc1234 |






<a name="v1-CancelTaskRequest"></a>

### CancelTaskRequest



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| task_id | [UUID](#v1-UUID) |  |  |






<a name="v1-CancelTaskResponse"></a>

### CancelTaskResponse



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| task | [Task](#v1-Task) |  |  |






<a name="v1-CheckScheduleConflictsRequest"></a>

### CheckScheduleConflictsRequest
CheckScheduleConflictsRequest checks whether a proposed scheduled operation
would conflict with any existing enabled schedules.
The operation oneof mirrors CreateTaskScheduleRequest: the target_spec
embedded in the operation message defines which racks are checked.

This call is advisory and intentionally coarse: it matches on operation
type and code only, without intersecting component-type filters or explicit
component UUID lists. As a result it may return false positives — two
schedules that target disjoint component sets on the same rack will appear
to conflict here even if their tasks would never collide at runtime.
Execution-time conflict detection (the task manager&#39;s conflict rules) remains
the authoritative backstop. The caller may proceed even when conflicts are
returned.


| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| operation | [ScheduledOperation](#v1-ScheduledOperation) |  |  |
| exclude_schedule_id | [UUID](#v1-UUID) | optional | exclude_schedule_id omits a schedule from the conflict check results. Pass the ID of the schedule being updated so its current definition is not returned as a conflict against the proposed replacement operation. |






<a name="v1-CheckScheduleConflictsResponse"></a>

### CheckScheduleConflictsResponse
CheckScheduleConflictsResponse lists the existing enabled schedules whose
operations may conflict with the proposed operation at execution time.
An empty list means no conflicts were detected.


| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| conflicts | [TaskSchedule](#v1-TaskSchedule) | repeated |  |






<a name="v1-Component"></a>

### Component



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| type | [ComponentType](#v1-ComponentType) |  |  |
| info | [DeviceInfo](#v1-DeviceInfo) |  |  |
| firmware_version | [string](#string) |  |  |
| position | [RackPosition](#v1-RackPosition) |  |  |
| bmcs | [BMCInfo](#v1-BMCInfo) | repeated |  |
| component_id | [string](#string) |  | Component&#39;s own ID from its source system (e.g., NICo machine_id for Compute) |
| rack_id | [UUID](#v1-UUID) |  |  |
| power_state | [string](#string) |  | Current power state (synced from external system by inventory loop) |
| status | [ComponentOperationStatus](#v1-ComponentOperationStatus) |  |  |
| leak_status | [LeakStatus](#v1-LeakStatus) |  | Coolant leak detection status (set by the leak-detection loop) |






<a name="v1-ComponentDiff"></a>

### ComponentDiff



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| type | [DiffType](#v1-DiffType) |  |  |
| component_id | [string](#string) |  | Component ID assigned by the component manager service |
| expected | [Component](#v1-Component) |  | Populated when type is MISSING |
| actual | [Component](#v1-Component) |  |  |
| field_diffs | [FieldDiff](#v1-FieldDiff) | repeated | Populated when type is MISMATCH |
| id | [UUID](#v1-UUID) |  | Flow internal component UUID |






<a name="v1-ComponentOperationStatus"></a>

### ComponentOperationStatus
ComponentOperationStatus is Flow&#39;s view of a component&#39;s operability. The
inventory loop computes it on every sync from core&#39;s controller_state.


| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| phase | [Phase](#v1-Phase) |  |  |
| reason | [string](#string) |  | Human-readable detail (typically the raw core state string). |
| blocked_operations | [OperationType](#v1-OperationType) | repeated | Operations Flow will reject while the component is in this status. Empty when phase is READY. |






<a name="v1-ComponentTarget"></a>

### ComponentTarget
ComponentTarget identifies a specific component


| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| id | [UUID](#v1-UUID) |  | Component UUID |
| external | [ExternalRef](#v1-ExternalRef) |  | External system reference |






<a name="v1-ComponentTargets"></a>

### ComponentTargets
ComponentTargets contains one or more component targets


| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| targets | [ComponentTarget](#v1-ComponentTarget) | repeated |  |






<a name="v1-ComponentTypes"></a>

### ComponentTypes
ComponentTypes contains one or more component type filters


| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| types | [ComponentType](#v1-ComponentType) | repeated |  |






<a name="v1-CreateExpectedRackRequest"></a>

### CreateExpectedRackRequest



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| rack | [Rack](#v1-Rack) |  |  |






<a name="v1-CreateExpectedRackResponse"></a>

### CreateExpectedRackResponse



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| id | [UUID](#v1-UUID) |  |  |






<a name="v1-CreateNVLDomainRequest"></a>

### CreateNVLDomainRequest



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| nvl_domain | [NVLDomain](#v1-NVLDomain) |  |  |






<a name="v1-CreateNVLDomainResponse"></a>

### CreateNVLDomainResponse



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| id | [UUID](#v1-UUID) |  |  |






<a name="v1-CreateOperationRuleRequest"></a>

### CreateOperationRuleRequest



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| name | [string](#string) |  |  |
| description | [string](#string) |  |  |
| operation_type | [OperationType](#v1-OperationType) |  |  |
| operation_code | [string](#string) |  | Specific operation code (e.g., &#34;power_on&#34;, &#34;upgrade&#34;) |
| rule_definition_json | [string](#string) |  | JSON-encoded RuleDefinition |
| is_default | [bool](#bool) |  |  |






<a name="v1-CreateOperationRuleResponse"></a>

### CreateOperationRuleResponse



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| id | [UUID](#v1-UUID) |  |  |






<a name="v1-CreateTaskScheduleRequest"></a>

### CreateTaskScheduleRequest
CreateTaskScheduleRequest creates a new TaskSchedule.
The target_spec on the operation message defines the initial scope; it follows
the same targeting rules as AddTaskScheduleScope (rack-level or component-level).
Use AddTaskScheduleScope / RemoveTaskScheduleScope to modify the scope after creation.


| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| schedule | [ScheduleConfig](#v1-ScheduleConfig) |  |  |
| operation | [ScheduledOperation](#v1-ScheduledOperation) |  |  |






<a name="v1-DeleteComponentRequest"></a>

### DeleteComponentRequest
DeleteComponent - soft-delete a single component by UUID


| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| id | [UUID](#v1-UUID) |  | Required: component UUID to delete |






<a name="v1-DeleteComponentResponse"></a>

### DeleteComponentResponse







<a name="v1-DeleteOperationRuleRequest"></a>

### DeleteOperationRuleRequest



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| rule_id | [UUID](#v1-UUID) |  |  |






<a name="v1-DeleteRackRequest"></a>

### DeleteRackRequest
DeleteRack - soft-delete a rack and cascade to its components


| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| id | [UUID](#v1-UUID) |  | Required: rack UUID to soft-delete |






<a name="v1-DeleteRackResponse"></a>

### DeleteRackResponse







<a name="v1-DeleteTaskScheduleRequest"></a>

### DeleteTaskScheduleRequest
DeleteTaskScheduleRequest permanently deletes a TaskSchedule and all its
scope entries. In-flight tasks are not cancelled.


| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| id | [UUID](#v1-UUID) |  |  |






<a name="v1-DetachRacksFromNVLDomainRequest"></a>

### DetachRacksFromNVLDomainRequest



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| rack_identifiers | [Identifier](#v1-Identifier) | repeated |  |






<a name="v1-DeviceInfo"></a>

### DeviceInfo



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| id | [UUID](#v1-UUID) |  |  |
| name | [string](#string) |  |  |
| manufacturer | [string](#string) |  |  |
| model | [string](#string) | optional |  |
| serial_number | [string](#string) |  |  |
| description | [string](#string) | optional |  |






<a name="v1-DeviceSerialInfo"></a>

### DeviceSerialInfo



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| manufacturer | [string](#string) |  |  |
| serial_number | [string](#string) |  |  |






<a name="v1-DisassociateRuleFromRackRequest"></a>

### DisassociateRuleFromRackRequest



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| rack_id | [UUID](#v1-UUID) |  |  |
| operation_type | [OperationType](#v1-OperationType) |  |  |
| operation_code | [string](#string) |  | Specific operation code |






<a name="v1-ExternalRef"></a>

### ExternalRef
ExternalRef identifies a component by its external system ID.
All component types are routed through Core (NICo); the ID is the
identifier expected by NICo for that component type (e.g. machine_id
for compute, PMC MAC for power shelf).


| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| type | [ComponentType](#v1-ComponentType) |  | Component type determines the source system |
| id | [string](#string) |  | ID expected by NICo for this component type |






<a name="v1-FieldDiff"></a>

### FieldDiff



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| field_name | [string](#string) |  | e.g., &#34;position.slot_id&#34;, &#34;firmware_version&#34; |
| expected_value | [string](#string) |  |  |
| actual_value | [string](#string) |  |  |






<a name="v1-Filter"></a>

### Filter
Filter represents a single filter condition


| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| rack_field | [RackFilterField](#v1-RackFilterField) |  | For rack queries |
| component_field | [ComponentFilterField](#v1-ComponentFilterField) |  | For component queries |
| query_info | [StringQueryInfo](#v1-StringQueryInfo) |  |  |






<a name="v1-GetComponentInfoByIDRequest"></a>

### GetComponentInfoByIDRequest



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| id | [UUID](#v1-UUID) |  |  |
| with_rack | [bool](#bool) |  |  |






<a name="v1-GetComponentInfoBySerialRequest"></a>

### GetComponentInfoBySerialRequest



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| serial_info | [DeviceSerialInfo](#v1-DeviceSerialInfo) |  |  |
| with_rack | [bool](#bool) |  |  |






<a name="v1-GetComponentInfoResponse"></a>

### GetComponentInfoResponse



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| component | [Component](#v1-Component) |  |  |
| rack | [Rack](#v1-Rack) |  |  |






<a name="v1-GetComponentsRequest"></a>

### GetComponentsRequest
GetComponents - retrieves components from local database


| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| target_spec | [OperationTargetSpec](#v1-OperationTargetSpec) | optional | Optional: Flexible targeting: rack(s) with optional type filter, or specific components. If not provided, queries all components. |
| filters | [Filter](#v1-Filter) | repeated | Filter conditions for component queries |
| pagination | [Pagination](#v1-Pagination) | optional |  |
| order_by | [OrderBy](#v1-OrderBy) | optional |  |






<a name="v1-GetComponentsResponse"></a>

### GetComponentsResponse



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| components | [Component](#v1-Component) | repeated |  |
| total | [int32](#int32) |  |  |






<a name="v1-GetListOfNVLDomainsRequest"></a>

### GetListOfNVLDomainsRequest



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| info | [StringQueryInfo](#v1-StringQueryInfo) |  |  |
| pagination | [Pagination](#v1-Pagination) | optional |  |






<a name="v1-GetListOfNVLDomainsResponse"></a>

### GetListOfNVLDomainsResponse



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| nvl_domains | [NVLDomain](#v1-NVLDomain) | repeated |  |
| total | [int32](#int32) |  |  |






<a name="v1-GetListOfRacksRequest"></a>

### GetListOfRacksRequest



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| filters | [Filter](#v1-Filter) | repeated | Filter conditions for rack queries |
| with_components | [bool](#bool) |  |  |
| pagination | [Pagination](#v1-Pagination) | optional |  |
| order_by | [OrderBy](#v1-OrderBy) | optional |  |






<a name="v1-GetListOfRacksResponse"></a>

### GetListOfRacksResponse



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| racks | [Rack](#v1-Rack) | repeated |  |
| total | [int32](#int32) |  |  |






<a name="v1-GetOperationRuleRequest"></a>

### GetOperationRuleRequest



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| rule_id | [UUID](#v1-UUID) |  |  |






<a name="v1-GetRackInfoByIDRequest"></a>

### GetRackInfoByIDRequest



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| id | [UUID](#v1-UUID) |  |  |
| with_components | [bool](#bool) |  |  |






<a name="v1-GetRackInfoBySerialRequest"></a>

### GetRackInfoBySerialRequest



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| serial_info | [DeviceSerialInfo](#v1-DeviceSerialInfo) |  |  |
| with_components | [bool](#bool) |  |  |






<a name="v1-GetRackInfoResponse"></a>

### GetRackInfoResponse



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| rack | [Rack](#v1-Rack) |  |  |






<a name="v1-GetRackRuleAssociationRequest"></a>

### GetRackRuleAssociationRequest



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| rack_id | [UUID](#v1-UUID) |  |  |
| operation_type | [OperationType](#v1-OperationType) |  |  |
| operation_code | [string](#string) |  | Specific operation code |






<a name="v1-GetRackRuleAssociationResponse"></a>

### GetRackRuleAssociationResponse



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| rule_id | [UUID](#v1-UUID) |  | Empty if no association exists |






<a name="v1-GetRacksForNVLDomainRequest"></a>

### GetRacksForNVLDomainRequest



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| nvl_domain_identifier | [Identifier](#v1-Identifier) |  |  |






<a name="v1-GetRacksForNVLDomainResponse"></a>

### GetRacksForNVLDomainResponse



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| racks | [Rack](#v1-Rack) | repeated |  |






<a name="v1-GetTaskScheduleRequest"></a>

### GetTaskScheduleRequest



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| id | [UUID](#v1-UUID) |  |  |






<a name="v1-GetTasksByIDsRequest"></a>

### GetTasksByIDsRequest



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| task_ids | [UUID](#v1-UUID) | repeated |  |






<a name="v1-GetTasksByIDsResponse"></a>

### GetTasksByIDsResponse



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| tasks | [Task](#v1-Task) | repeated |  |






<a name="v1-Identifier"></a>

### Identifier



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| id | [UUID](#v1-UUID) |  |  |
| name | [string](#string) |  |  |






<a name="v1-IngestRackRequest"></a>

### IngestRackRequest



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| target_spec | [OperationTargetSpec](#v1-OperationTargetSpec) |  | Target racks for ingestion |
| filters | [Filter](#v1-Filter) | repeated | Filter conditions for component queries (e.g. by type, name) |
| description | [string](#string) |  | optional task description |
| rule_id | [UUID](#v1-UUID) | optional | optional: override rule resolution with a specific rule |






<a name="v1-ListOperationRulesRequest"></a>

### ListOperationRulesRequest



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| operation_type | [OperationType](#v1-OperationType) | optional |  |
| is_default | [bool](#bool) | optional |  |
| offset | [int32](#int32) | optional |  |
| limit | [int32](#int32) | optional |  |






<a name="v1-ListOperationRulesResponse"></a>

### ListOperationRulesResponse



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| rules | [OperationRule](#v1-OperationRule) | repeated |  |
| total_count | [int32](#int32) |  |  |






<a name="v1-ListRackRuleAssociationsRequest"></a>

### ListRackRuleAssociationsRequest



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| rack_id | [UUID](#v1-UUID) |  |  |






<a name="v1-ListRackRuleAssociationsResponse"></a>

### ListRackRuleAssociationsResponse



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| associations | [RackRuleAssociation](#v1-RackRuleAssociation) | repeated |  |






<a name="v1-ListTaskScheduleScopesRequest"></a>

### ListTaskScheduleScopesRequest
ListTaskScheduleScopesRequest returns all scope entries for a given schedule.


| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| schedule_id | [UUID](#v1-UUID) |  |  |






<a name="v1-ListTaskScheduleScopesResponse"></a>

### ListTaskScheduleScopesResponse



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| scopes | [TaskScheduleScope](#v1-TaskScheduleScope) | repeated |  |






<a name="v1-ListTaskSchedulesRequest"></a>

### ListTaskSchedulesRequest
ListTaskSchedulesRequest lists TaskSchedules with optional filters.
Results are ordered by creation time ascending.


| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| rack_id | [UUID](#v1-UUID) | optional | if set, return only schedules with a scope on this rack |
| pagination | [Pagination](#v1-Pagination) | optional |  |
| enabled_only | [bool](#bool) | optional | if true, return only enabled (non-paused) schedules |






<a name="v1-ListTaskSchedulesResponse"></a>

### ListTaskSchedulesResponse



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| task_schedules | [TaskSchedule](#v1-TaskSchedule) | repeated |  |
| total | [int32](#int32) |  | total matching count before pagination |






<a name="v1-ListTasksRequest"></a>

### ListTasksRequest
ListTasks - list Tasks with optional filters.

Filters compose with AND: a Task is returned only if it satisfies every
set filter. Unset optional fields are not applied; with no filter set
every Task is returned subject to pagination.


| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| rack_id | [UUID](#v1-UUID) | optional | Restrict to Tasks created against this rack. |
| active_only | [bool](#bool) |  | Restrict to non-terminal Tasks (Waiting, Pending, Running). |
| pagination | [Pagination](#v1-Pagination) | optional |  |
| component_id | [UUID](#v1-UUID) | optional | Restrict to Tasks that target this component UUID, regardless of component type. A rack_id &#43; component_id combination that references a component not on the given rack is not an error; it yields an empty result. |
| with_report | [bool](#bool) |  | When true, populate Task.report on each returned task. Defaults to false because report bodies can be several KB and would otherwise be persisted in every Temporal activity / workflow result payload along the caller&#39;s path even when the caller never reads them. GetTasksByIDs and CancelTask always return the report and do not accept this flag. |






<a name="v1-ListTasksResponse"></a>

### ListTasksResponse



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| tasks | [Task](#v1-Task) | repeated |  |
| total | [int32](#int32) |  |  |






<a name="v1-Location"></a>

### Location



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| region | [string](#string) |  |  |
| datacenter | [string](#string) |  |  |
| room | [string](#string) |  |  |
| position | [string](#string) |  |  |






<a name="v1-NVLDomain"></a>

### NVLDomain



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| identifier | [Identifier](#v1-Identifier) |  |  |






<a name="v1-OperationRule"></a>

### OperationRule



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| id | [UUID](#v1-UUID) |  |  |
| name | [string](#string) |  |  |
| description | [string](#string) |  |  |
| operation_type | [OperationType](#v1-OperationType) |  |  |
| operation_code | [string](#string) |  | Specific operation code (e.g., &#34;power_on&#34;, &#34;upgrade&#34;) |
| rule_definition_json | [string](#string) |  | JSON-encoded RuleDefinition |
| is_default | [bool](#bool) |  |  |
| created_at | [google.protobuf.Timestamp](#google-protobuf-Timestamp) |  |  |
| updated_at | [google.protobuf.Timestamp](#google-protobuf-Timestamp) |  |  |






<a name="v1-OperationTargetSpec"></a>

### OperationTargetSpec
OperationTargetSpec contains targets for an operation.
Supports either rack-level targeting (with optional type filtering)
or component-level targeting (by UUID or external reference), but not both.


| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| racks | [RackTargets](#v1-RackTargets) |  |  |
| components | [ComponentTargets](#v1-ComponentTargets) |  |  |






<a name="v1-OrderBy"></a>

### OrderBy
OrderBy represents ordering specification


| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| rack_field | [RackOrderByField](#v1-RackOrderByField) |  | For rack queries |
| component_field | [ComponentOrderByField](#v1-ComponentOrderByField) |  | For component queries |
| direction | [string](#string) |  | ASC or DESC |






<a name="v1-Pagination"></a>

### Pagination



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| offset | [int32](#int32) |  |  |
| limit | [int32](#int32) |  |  |






<a name="v1-PatchComponentRequest"></a>

### PatchComponentRequest
PatchComponent - update a single component&#39;s fields


| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| id | [UUID](#v1-UUID) |  | Required: component UUID |
| firmware_version | [string](#string) | optional | Update firmware version |
| position | [RackPosition](#v1-RackPosition) | optional | Update slot_id, tray_idx, host_id |
| description | [string](#string) | optional | Update description (JSON string) |
| rack_id | [UUID](#v1-UUID) | optional | Re-assign to a different rack |
| bmcs | [BMCInfo](#v1-BMCInfo) | repeated | Update BMCs (matched by MAC address; create if new) |
| update_mask | [google.protobuf.FieldMask](https://protobuf.dev/reference/protobuf/google.protobuf/) |  | Optional for backward compatibility. When omitted, position replaces all three coordinates. When set, supported paths are position.slot_id, position.tray_idx, and position.host_id; only those coordinates change. |






<a name="v1-PatchComponentResponse"></a>

### PatchComponentResponse



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| component | [Component](#v1-Component) |  | The updated component |






<a name="v1-PatchRackRequest"></a>

### PatchRackRequest



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| rack | [Rack](#v1-Rack) |  |  |






<a name="v1-PatchRackResponse"></a>

### PatchRackResponse



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| report | [string](#string) |  |  |






<a name="v1-PauseTaskScheduleRequest"></a>

### PauseTaskScheduleRequest
PauseTaskScheduleRequest disables a TaskSchedule without deleting it.
The schedule will not fire until resumed. Has no effect if already paused.
Returns an error for a one-time schedule that has already fired.


| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| id | [UUID](#v1-UUID) |  |  |






<a name="v1-PowerOffRackRequest"></a>

### PowerOffRackRequest



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| target_spec | [OperationTargetSpec](#v1-OperationTargetSpec) |  | Flexible targeting: rack(s) with optional type filter, or specific components |
| forced | [bool](#bool) |  |  |
| description | [string](#string) |  | optional task description |
| queue_options | [QueueOptions](#v1-QueueOptions) | optional |  |
| rule_id | [UUID](#v1-UUID) | optional | optional: override rule resolution with a specific rule |
| override_readiness_check | [bool](#bool) |  | When true, proceed with the power-off even if one or more target components (or, for rack-scoped components, any host on the owning rack) are reported as not ready for the operation by their persisted ComponentOperationStatus. Intended for operator-supervised maintenance where tenant impact has been acknowledged out-of-band; the bypass is recorded in the server log. |






<a name="v1-PowerOnRackRequest"></a>

### PowerOnRackRequest



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| target_spec | [OperationTargetSpec](#v1-OperationTargetSpec) |  | Flexible targeting: rack(s) with optional type filter, or specific components |
| description | [string](#string) |  | optional task description |
| queue_options | [QueueOptions](#v1-QueueOptions) | optional |  |
| rule_id | [UUID](#v1-UUID) | optional | optional: override rule resolution with a specific rule |
| override_readiness_check | [bool](#bool) |  | When true, proceed with the power-on even if one or more target components (or, for rack-scoped components, any host on the owning rack) are reported as not ready for the operation by their persisted ComponentOperationStatus. Intended for operator-supervised maintenance where tenant impact has been acknowledged out-of-band; the bypass is recorded in the server log. |






<a name="v1-PowerResetRackRequest"></a>

### PowerResetRackRequest



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| target_spec | [OperationTargetSpec](#v1-OperationTargetSpec) |  | Flexible targeting: rack(s) with optional type filter, or specific components |
| forced | [bool](#bool) |  |  |
| description | [string](#string) |  | optional task description |
| queue_options | [QueueOptions](#v1-QueueOptions) | optional |  |
| rule_id | [UUID](#v1-UUID) | optional | optional: override rule resolution with a specific rule |
| override_readiness_check | [bool](#bool) |  | When true, proceed with the reset even if one or more target components (or, for rack-scoped components, any host on the owning rack) are reported as not ready for the operation by their persisted ComponentOperationStatus. Intended for operator-supervised maintenance where tenant impact has been acknowledged out-of-band; the bypass is recorded in the server log. |






<a name="v1-PurgeComponentRequest"></a>

### PurgeComponentRequest
PurgeComponent - permanently remove a soft-deleted component


| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| id | [UUID](#v1-UUID) |  | Required: component UUID to purge (must already be soft-deleted) |






<a name="v1-PurgeComponentResponse"></a>

### PurgeComponentResponse







<a name="v1-PurgeRackRequest"></a>

### PurgeRackRequest
PurgeRack - permanently remove a soft-deleted rack and its components


| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| id | [UUID](#v1-UUID) |  | Required: rack UUID to purge (must already be soft-deleted) |






<a name="v1-PurgeRackResponse"></a>

### PurgeRackResponse







<a name="v1-QueueOptions"></a>

### QueueOptions
QueueOptions controls how a task behaves when a conflict is detected.


| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| conflict_strategy | [ConflictStrategy](#v1-ConflictStrategy) |  | How to handle the task when a conflict is detected. Defaults to CONFLICT_STRATEGY_REJECT (wire value 0). |
| queue_timeout_seconds | [int32](#int32) |  | How long (seconds) to wait in queue before expiring. 0 means use the server default (~1h). Only relevant when conflict_strategy is CONFLICT_STRATEGY_QUEUE. |






<a name="v1-Rack"></a>

### Rack



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| info | [DeviceInfo](#v1-DeviceInfo) |  |  |
| location | [Location](#v1-Location) |  |  |
| components | [Component](#v1-Component) | repeated |  |






<a name="v1-RackPosition"></a>

### RackPosition



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| slot_id | [int32](#int32) |  |  |
| tray_idx | [int32](#int32) |  |  |
| host_id | [int32](#int32) |  |  |






<a name="v1-RackRuleAssociation"></a>

### RackRuleAssociation



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| rack_id | [UUID](#v1-UUID) |  |  |
| operation_type | [OperationType](#v1-OperationType) |  |  |
| operation_code | [string](#string) |  | Specific operation code (e.g., &#34;power_on&#34;, &#34;upgrade&#34;) |
| rule_id | [UUID](#v1-UUID) |  |  |
| created_at | [google.protobuf.Timestamp](#google-protobuf-Timestamp) |  |  |
| updated_at | [google.protobuf.Timestamp](#google-protobuf-Timestamp) |  |  |






<a name="v1-RackTarget"></a>

### RackTarget
RackTarget identifies a rack and optionally filters by component type.
To target specific components, use the component-level APIs instead.


| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| id | [UUID](#v1-UUID) |  | Rack UUID |
| name | [string](#string) |  | Rack name |
| component_types | [ComponentType](#v1-ComponentType) | repeated | Optional: filter by component type. Omit (or send empty list) to include all components in the rack. |






<a name="v1-RackTargets"></a>

### RackTargets
RackTargets contains one or more rack targets


| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| targets | [RackTarget](#v1-RackTarget) | repeated |  |






<a name="v1-RemoveTaskScheduleScopeRequest"></a>

### RemoveTaskScheduleScopeRequest
RemoveTaskScheduleScopeRequest removes a single rack scope entry by its scope ID.
In-flight tasks for that rack are not cancelled.


| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| scope_id | [UUID](#v1-UUID) |  |  |






<a name="v1-ResumeTaskScheduleRequest"></a>

### ResumeTaskScheduleRequest
ResumeTaskScheduleRequest re-enables a paused TaskSchedule. For interval
and cron schedules, next_run_at is recomputed from the current time so the
schedule does not fire immediately. Has no effect if already enabled.


| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| id | [UUID](#v1-UUID) |  |  |






<a name="v1-ScheduleConfig"></a>

### ScheduleConfig
ScheduleConfig groups the scheduling fields shared by multiple request types.


| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| name | [string](#string) |  |  |
| spec | [ScheduleSpec](#v1-ScheduleSpec) |  |  |
| overlap_policy | [OverlapPolicy](#v1-OverlapPolicy) |  |  |






<a name="v1-ScheduleSpec"></a>

### ScheduleSpec



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| type | [ScheduleSpecType](#v1-ScheduleSpecType) |  |  |
| spec | [string](#string) |  |  |
| timezone | [string](#string) |  | IANA timezone for interpreting cron specs (e.g. &#34;America/New_York&#34;). Defaults to &#34;UTC&#34;. Ignored for interval and one-time specs. |






<a name="v1-ScheduledOperation"></a>

### ScheduledOperation
ScheduledOperation is the shared operation oneof used by
CreateTaskScheduleRequest and CheckScheduleConflictsRequest.
Centralising it here means a single proto change adds support for a new
operation type in both RPCs, and the Go conversion logic lives in one place.

Note: the embedded request messages (e.g. PowerOnRackRequest) may carry a
description field, but it is ignored when used inside a ScheduledOperation.
The dispatcher generates task descriptions automatically at fire time in the
form &#34;&lt;schedule name&gt; — &lt;RFC3339 timestamp&gt;&#34;.


| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| power_on | [PowerOnRackRequest](#v1-PowerOnRackRequest) |  |  |
| power_off | [PowerOffRackRequest](#v1-PowerOffRackRequest) |  |  |
| power_reset | [PowerResetRackRequest](#v1-PowerResetRackRequest) |  |  |
| bring_up | [BringUpRackRequest](#v1-BringUpRackRequest) |  |  |
| upgrade_firmware | [UpgradeFirmwareRequest](#v1-UpgradeFirmwareRequest) |  |  |
| ingest | [IngestRackRequest](#v1-IngestRackRequest) |  |  |






<a name="v1-SetRuleAsDefaultRequest"></a>

### SetRuleAsDefaultRequest



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| rule_id | [UUID](#v1-UUID) |  |  |






<a name="v1-StringQueryInfo"></a>

### StringQueryInfo



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| patterns | [string](#string) | repeated |  |
| is_wildcard | [bool](#bool) |  |  |
| use_or | [bool](#bool) |  |  |






<a name="v1-SubmitTaskResponse"></a>

### SubmitTaskResponse



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| task_ids | [UUID](#v1-UUID) | repeated | Multiple task IDs (1 task per rack) |






<a name="v1-Task"></a>

### Task



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| id | [UUID](#v1-UUID) |  |  |
| operation | [string](#string) |  |  |
| rack_id | [UUID](#v1-UUID) |  |  |
| component_uuids | [UUID](#v1-UUID) | repeated |  |
| description | [string](#string) |  | description is provided by the client when the task is created. |
| executor_type | [TaskExecutorType](#v1-TaskExecutorType) |  |  |
| execution_id | [string](#string) |  |  |
| status | [TaskStatus](#v1-TaskStatus) |  |  |
| message | [string](#string) |  | message is brief text tied to status (not execution progress). |
| queue_expires_at | [google.protobuf.Timestamp](#google-protobuf-Timestamp) | optional | queue_expires_at is set only for waiting tasks; absent for all other statuses. |
| created_at | [google.protobuf.Timestamp](#google-protobuf-Timestamp) |  |  |
| finished_at | [google.protobuf.Timestamp](#google-protobuf-Timestamp) | optional |  |
| applied_rule_id | [UUID](#v1-UUID) | optional |  |
| updated_at | [google.protobuf.Timestamp](#google-protobuf-Timestamp) |  |  |
| started_at | [google.protobuf.Timestamp](#google-protobuf-Timestamp) | optional |  |
| report | [string](#string) |  | report is a versioned JSON document with structured execution progress. |






<a name="v1-TaskSchedule"></a>

### TaskSchedule
TaskSchedule defines when (spec) and what (operation) should run automatically.
Which racks to target is tracked separately in TaskScheduleScope rows and
managed via AddTaskScheduleScope / RemoveTaskScheduleScope / ListTaskScheduleScopes.


| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| id | [UUID](#v1-UUID) |  |  |
| name | [string](#string) |  | unique, human-readable identifier |
| spec | [ScheduleSpec](#v1-ScheduleSpec) |  | when to fire (interval, cron, or one-time) |
| overlap_policy | [OverlapPolicy](#v1-OverlapPolicy) |  |  |
| enabled | [bool](#bool) |  | false = paused (will not fire) |
| next_run_at | [google.protobuf.Timestamp](#google-protobuf-Timestamp) | optional | absent for disabled or fully-fired one-time schedules |
| last_run_at | [google.protobuf.Timestamp](#google-protobuf-Timestamp) | optional | absent if the schedule has never fired |
| created_at | [google.protobuf.Timestamp](#google-protobuf-Timestamp) |  |  |
| updated_at | [google.protobuf.Timestamp](#google-protobuf-Timestamp) |  |  |
| operation_type | [string](#string) |  | operation_type identifies the kind of operation this schedule runs. Values: &#34;POWER_ON&#34;, &#34;POWER_OFF&#34;, &#34;POWER_RESET&#34;, &#34;BRING_UP&#34;, &#34;INGEST&#34;, &#34;UPGRADE_FIRMWARE&#34;, &#34;DOWNGRADE_FIRMWARE&#34;, &#34;ROLLBACK_FIRMWARE&#34;. |
| description | [string](#string) |  | description is a human-readable summary of the operation and its key parameters, e.g. &#34;Power Reset (forced)&#34; or &#34;Upgrade Firmware to v2.3.1&#34;. |






<a name="v1-TaskScheduleScope"></a>

### TaskScheduleScope
TaskScheduleScope represents one rack target in a schedule&#39;s scope.
Each scope entry causes one task to be submitted per schedule firing.
last_task_id tracks the task produced for this rack by the most recent firing;
the dispatcher uses it for the overlap check. Absent if no task has fired yet
for this scope (e.g. a newly added rack).


| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| id | [UUID](#v1-UUID) |  |  |
| schedule_id | [UUID](#v1-UUID) |  |  |
| rack_id | [UUID](#v1-UUID) |  |  |
| types | [ComponentTypes](#v1-ComponentTypes) |  | types filters by component type (e.g. COMPUTE, POWERSHELF). |
| components | [ComponentTargets](#v1-ComponentTargets) |  | components targets specific components by UUID or external reference. |
| last_task_id | [UUID](#v1-UUID) |  | absent until the first firing for this scope |
| created_at | [google.protobuf.Timestamp](#google-protobuf-Timestamp) |  |  |






<a name="v1-TriggerTaskScheduleRequest"></a>

### TriggerTaskScheduleRequest
TriggerTaskScheduleRequest fires a TaskSchedule immediately, regardless of
next_run_at or enabled state. The overlap policy is not consulted — all
scopes are submitted unconditionally. Returns an error for a one-time
schedule that has already fired.


| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| id | [UUID](#v1-UUID) |  |  |






<a name="v1-UUID"></a>

### UUID



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| id | [string](#string) |  |  |






<a name="v1-UpdateOperationRuleRequest"></a>

### UpdateOperationRuleRequest



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| rule_id | [UUID](#v1-UUID) |  |  |
| name | [string](#string) | optional |  |
| description | [string](#string) | optional |  |
| rule_definition_json | [string](#string) | optional | JSON-encoded RuleDefinition |






<a name="v1-UpdateTaskScheduleRequest"></a>

### UpdateTaskScheduleRequest
UpdateTaskScheduleRequest updates the scheduling config of an existing
TaskSchedule. To modify which racks are targeted, use
AddTaskScheduleScope / RemoveTaskScheduleScope instead.

update_mask is required and controls which fields are written. Supported paths:
  &#34;schedule.name&#34;           – display name
  &#34;schedule.overlap_policy&#34; – overlap behaviour
  &#34;schedule.spec&#34;           – full spec block (type &#43; spec string &#43; next_run_at recomputed)
  &#34;schedule.spec.timezone&#34;  – timezone only (spec type/string unchanged)


| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| id | [UUID](#v1-UUID) |  |  |
| schedule | [ScheduleConfig](#v1-ScheduleConfig) |  |  |
| update_mask | [google.protobuf.FieldMask](#google-protobuf-FieldMask) |  |  |






<a name="v1-UpdateTaskScheduleScopeRequest"></a>

### UpdateTaskScheduleScopeRequest
UpdateTaskScheduleScopeRequest reconciles the schedule&#39;s scope against the
desired target_spec: racks present in desired_scope but not in the current scope
are added; racks present in the current scope but absent from desired_scope are
removed; racks present in both have their component_filter updated if changed.
For component-level targets the server resolves rack membership automatically.


| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| schedule_id | [UUID](#v1-UUID) |  |  |
| desired_scope | [OperationTargetSpec](#v1-OperationTargetSpec) |  |  |






<a name="v1-UpdateTaskScheduleScopeResponse"></a>

### UpdateTaskScheduleScopeResponse
UpdateTaskScheduleScopeResponse returns the complete scope after reconciliation.


| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| scopes | [TaskScheduleScope](#v1-TaskScheduleScope) | repeated |  |
| added | [int32](#int32) |  | number of scope entries added |
| removed | [int32](#int32) |  | number of scope entries removed |
| updated | [int32](#int32) |  | number of scope entries with updated component_filter |






<a name="v1-UpgradeFirmwareRequest"></a>

### UpgradeFirmwareRequest



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| target_spec | [OperationTargetSpec](#v1-OperationTargetSpec) |  | required: identifies components to upgrade |
| target_version | [string](#string) | optional | optional: target firmware version |
| start_time | [google.protobuf.Timestamp](#google-protobuf-Timestamp) | optional | optional: scheduled start time |
| end_time | [google.protobuf.Timestamp](#google-protobuf-Timestamp) | optional | optional: scheduled end time |
| description | [string](#string) |  | optional: task description |
| queue_options | [QueueOptions](#v1-QueueOptions) | optional |  |
| rule_id | [UUID](#v1-UUID) | optional | optional: override rule resolution with a specific rule |
| sub_targets | [string](#string) | repeated | Optional subset of firmware sub-parts to update within each tray selected by target_spec, e.g. [&#34;bmc&#34;, &#34;nvos&#34;] for switch trays or [&#34;psu&#34;] for powershelf trays. Named &#34;sub_targets&#34; (not &#34;components&#34;) to avoid colliding with OperationTargetSpec.components, which selects tray INSTANCES rather than sub-parts of a tray. Names are lowercase. Empty or omitted means update everything in the bundle (current default behavior). Unknown names are rejected by the downstream component manager. |
| override_readiness_check | [bool](#bool) |  | When true, proceed with the firmware update even if one or more target components (or, for rack-scoped components, any host on the owning rack) are reported as not ready for the operation by their persisted ComponentOperationStatus. The flag is intended for operator- supervised maintenance windows where the tenant impact has been acknowledged out-of-band; setting it bypasses the readiness gate that would otherwise block disruptive operations against tenanted hardware. The bypass is recorded in the server log. |






<a name="v1-ValidateComponentsRequest"></a>

### ValidateComponentsRequest



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| target_spec | [OperationTargetSpec](#v1-OperationTargetSpec) | optional | Optional: Flexible targeting: rack(s) with optional type filter, or specific components. If not provided, returns all diffs. |
| filters | [Filter](#v1-Filter) | repeated | Filter conditions for component queries |
| pagination | [Pagination](#v1-Pagination) | optional |  |
| order_by | [OrderBy](#v1-OrderBy) | optional |  |






<a name="v1-ValidateComponentsResponse"></a>

### ValidateComponentsResponse



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| diffs | [ComponentDiff](#v1-ComponentDiff) | repeated |  |
| total_diffs | [int32](#int32) |  |  |
| missing_count | [int32](#int32) |  | Summary counts

Expected by Flow but not found in the component manager service |
| unexpected_count | [int32](#int32) |  | Found in the component manager service but not expected by Flow |
| mismatch_count | [int32](#int32) |  | In both but with field differences |
| match_count | [int32](#int32) |  |  |






<a name="v1-VersionRequest"></a>

### VersionRequest
Version API messages





 


<a name="v1-BMCType"></a>

### BMCType


| Name | Number | Description |
| ---- | ------ | ----------- |
| BMC_TYPE_UNKNOWN | 0 |  |
| BMC_TYPE_HOST | 1 |  |
| BMC_TYPE_DPU | 2 |  |



<a name="v1-ComponentFilterField"></a>

### ComponentFilterField
ComponentFilterField represents the supported filter field types for component queries

| Name | Number | Description |
| ---- | ------ | ----------- |
| COMPONENT_FILTER_FIELD_UNSPECIFIED | 0 |  |
| COMPONENT_FILTER_FIELD_NAME | 1 | Filter by component name |
| COMPONENT_FILTER_FIELD_MANUFACTURER | 2 | Filter by manufacturer |
| COMPONENT_FILTER_FIELD_MODEL | 3 | Filter by model (stored in description JSONB) |
| COMPONENT_FILTER_FIELD_TYPE | 4 | Filter by component type (use ComponentType enum string values in StringQueryInfo) |



<a name="v1-ComponentOrderByField"></a>

### ComponentOrderByField
ComponentOrderByField represents the supported order by field types for component queries

| Name | Number | Description |
| ---- | ------ | ----------- |
| COMPONENT_ORDER_BY_FIELD_UNSPECIFIED | 0 |  |
| COMPONENT_ORDER_BY_FIELD_NAME | 1 | Order by component name |
| COMPONENT_ORDER_BY_FIELD_MANUFACTURER | 2 | Order by manufacturer |
| COMPONENT_ORDER_BY_FIELD_MODEL | 3 | Order by model |
| COMPONENT_ORDER_BY_FIELD_TYPE | 4 | Order by component type |



<a name="v1-ComponentType"></a>

### ComponentType


| Name | Number | Description |
| ---- | ------ | ----------- |
| COMPONENT_TYPE_UNKNOWN | 0 |  |
| COMPONENT_TYPE_COMPUTE | 1 |  |
| COMPONENT_TYPE_NVSWITCH | 2 |  |
| COMPONENT_TYPE_POWERSHELF | 3 |  |
| COMPONENT_TYPE_TORSWITCH | 4 |  |
| COMPONENT_TYPE_UMS | 5 |  |
| COMPONENT_TYPE_CDU | 6 |  |



<a name="v1-ConflictStrategy"></a>

### ConflictStrategy
ConflictStrategy controls how a task behaves when a conflict is detected.

| Name | Number | Description |
| ---- | ------ | ----------- |
| CONFLICT_STRATEGY_UNSPECIFIED | 0 | CONFLICT_STRATEGY_UNSPECIFIED defaults to REJECT. Wire value 0 preserves backward compatibility with the former bool false (reject). |
| CONFLICT_STRATEGY_QUEUE | 1 | CONFLICT_STRATEGY_QUEUE queues the task until the conflicting task completes. Wire value 1 preserves backward compatibility with the former bool true (queue). |
| CONFLICT_STRATEGY_REJECT | 2 | CONFLICT_STRATEGY_REJECT immediately rejects the task when a conflict is detected. |



<a name="v1-DiffType"></a>

### DiffType


| Name | Number | Description |
| ---- | ------ | ----------- |
| DIFF_TYPE_UNKNOWN | 0 |  |
| DIFF_TYPE_MISSING | 1 | Expected by Flow but not found in the component manager service |
| DIFF_TYPE_UNEXPECTED | 2 | Found in the component manager service but not expected by Flow |
| DIFF_TYPE_MISMATCH | 3 | In both but with field differences |



<a name="v1-LeakStatus"></a>

### LeakStatus
LeakStatus is Flow&#39;s view of whether coolant leak detection has fired for
a component. The leak-detection loop sets it from core&#39;s tray-leak-detection
health alert; LEAK_STATUS_UNKNOWN is the resting value for components the
loop has not yet evaluated.

| Name | Number | Description |
| ---- | ------ | ----------- |
| LEAK_STATUS_UNKNOWN | 0 |  |
| LEAK_STATUS_DETECTED | 1 |  |
| LEAK_STATUS_NOT_DETECTED | 2 |  |



<a name="v1-OperationType"></a>

### OperationType


| Name | Number | Description |
| ---- | ------ | ----------- |
| OPERATION_TYPE_UNKNOWN | 0 |  |
| OPERATION_TYPE_POWER_CONTROL | 1 |  |
| OPERATION_TYPE_FIRMWARE_CONTROL | 2 |  |



<a name="v1-OverlapPolicy"></a>

### OverlapPolicy
OverlapPolicy controls what happens when a schedule fires while the previous
execution for the same scope is still active.

| Name | Number | Description |
| ---- | ------ | ----------- |
| OVERLAP_POLICY_UNSPECIFIED | 0 |  |
| OVERLAP_POLICY_SKIP | 1 | skip this firing cycle for any scope whose last task is still active |
| OVERLAP_POLICY_QUEUE | 2 | submit unconditionally; the task manager queues behind the active task |



<a name="v1-Phase"></a>

### Phase
Phase is the coarse lifecycle bucket a component is in, derived from
core&#39;s per-component state machine. Shared across compute, nvswitch,
and power shelf.

| Name | Number | Description |
| ---- | ------ | ----------- |
| PHASE_UNKNOWN | 0 |  |
| PHASE_INITIALIZING | 1 |  |
| PHASE_READY | 2 |  |
| PHASE_IN_USE | 3 |  |
| PHASE_ERROR | 4 |  |
| PHASE_DELETING | 5 |  |



<a name="v1-PowerControlOp"></a>

### PowerControlOp


| Name | Number | Description |
| ---- | ------ | ----------- |
| POWER_CONTROL_OP_UNKNOWN | 0 |  |
| POWER_CONTROL_OP_ON | 1 | Power On |
| POWER_CONTROL_OP_FORCE_ON | 2 |  |
| POWER_CONTROL_OP_OFF | 3 | Power Off

graceful shutdown |
| POWER_CONTROL_OP_FORCE_OFF | 4 |  |
| POWER_CONTROL_OP_RESTART | 5 | Restart (OS level reboot)

graceful restart |
| POWER_CONTROL_OP_FORCE_RESTART | 6 |  |
| POWER_CONTROL_OP_WARM_RESET | 7 | Reset (hardware level) |
| POWER_CONTROL_OP_COLD_RESET | 8 |  |



<a name="v1-RackFilterField"></a>

### RackFilterField
RackFilterField represents the supported filter field types for rack queries

| Name | Number | Description |
| ---- | ------ | ----------- |
| RACK_FILTER_FIELD_UNSPECIFIED | 0 |  |
| RACK_FILTER_FIELD_NAME | 1 | Filter by rack name |
| RACK_FILTER_FIELD_MANUFACTURER | 2 | Filter by manufacturer |
| RACK_FILTER_FIELD_MODEL | 3 | Filter by model (stored in description JSONB) |



<a name="v1-RackOrderByField"></a>

### RackOrderByField
RackOrderByField represents the supported order by field types for rack queries

| Name | Number | Description |
| ---- | ------ | ----------- |
| RACK_ORDER_BY_FIELD_UNSPECIFIED | 0 |  |
| RACK_ORDER_BY_FIELD_NAME | 1 | Order by rack name |
| RACK_ORDER_BY_FIELD_MANUFACTURER | 2 | Order by manufacturer |
| RACK_ORDER_BY_FIELD_MODEL | 3 | Order by model |



<a name="v1-ScheduleSpecType"></a>

### ScheduleSpecType


| Name | Number | Description |
| ---- | ------ | ----------- |
| SCHEDULE_SPEC_TYPE_UNSPECIFIED | 0 |  |
| SCHEDULE_SPEC_TYPE_INTERVAL | 1 | spec is a Go duration string, e.g. &#34;24h&#34; |
| SCHEDULE_SPEC_TYPE_CRON | 2 | spec is a 5-field cron expression |
| SCHEDULE_SPEC_TYPE_ONE_TIME | 3 | spec is an RFC3339 timestamp |



<a name="v1-TaskExecutorType"></a>

### TaskExecutorType


| Name | Number | Description |
| ---- | ------ | ----------- |
| TASK_EXECUTOR_TYPE_UNKNOWN | 0 |  |
| TASK_EXECUTOR_TYPE_TEMPORAL | 1 |  |



<a name="v1-TaskStatus"></a>

### TaskStatus


| Name | Number | Description |
| ---- | ------ | ----------- |
| TASK_STATUS_UNKNOWN | 0 |  |
| TASK_STATUS_PENDING | 1 |  |
| TASK_STATUS_RUNNING | 2 |  |
| TASK_STATUS_COMPLETED | 3 |  |
| TASK_STATUS_FAILED | 4 |  |
| TASK_STATUS_TERMINATED | 5 |  |
| TASK_STATUS_WAITING | 6 | TASK_STATUS_WAITING means the task was queued because a conflicting task is active on the rack. It will be promoted automatically when the rack becomes available, or can be cancelled explicitly via CancelTask. |


 

 


<a name="v1-Flow"></a>

### Flow


| Method Name | Request Type | Response Type | Description |
| ----------- | ------------ | ------------- | ------------|
| Version | [VersionRequest](#v1-VersionRequest) | [BuildInfo](#v1-BuildInfo) | Version |
| CreateTaskSchedule | [CreateTaskScheduleRequest](#v1-CreateTaskScheduleRequest) | [TaskSchedule](#v1-TaskSchedule) | Task schedules |
| GetTaskSchedule | [GetTaskScheduleRequest](#v1-GetTaskScheduleRequest) | [TaskSchedule](#v1-TaskSchedule) |  |
| ListTaskSchedules | [ListTaskSchedulesRequest](#v1-ListTaskSchedulesRequest) | [ListTaskSchedulesResponse](#v1-ListTaskSchedulesResponse) |  |
| UpdateTaskSchedule | [UpdateTaskScheduleRequest](#v1-UpdateTaskScheduleRequest) | [TaskSchedule](#v1-TaskSchedule) |  |
| PauseTaskSchedule | [PauseTaskScheduleRequest](#v1-PauseTaskScheduleRequest) | [TaskSchedule](#v1-TaskSchedule) |  |
| ResumeTaskSchedule | [ResumeTaskScheduleRequest](#v1-ResumeTaskScheduleRequest) | [TaskSchedule](#v1-TaskSchedule) |  |
| DeleteTaskSchedule | [DeleteTaskScheduleRequest](#v1-DeleteTaskScheduleRequest) | [.google.protobuf.Empty](#google-protobuf-Empty) |  |
| TriggerTaskSchedule | [TriggerTaskScheduleRequest](#v1-TriggerTaskScheduleRequest) | [SubmitTaskResponse](#v1-SubmitTaskResponse) |  |
| AddTaskScheduleScope | [AddTaskScheduleScopeRequest](#v1-AddTaskScheduleScopeRequest) | [AddTaskScheduleScopeResponse](#v1-AddTaskScheduleScopeResponse) | add one or more racks to a schedule&#39;s scope |
| RemoveTaskScheduleScope | [RemoveTaskScheduleScopeRequest](#v1-RemoveTaskScheduleScopeRequest) | [.google.protobuf.Empty](#google-protobuf-Empty) | remove a single rack from a schedule&#39;s scope by scope ID |
| UpdateTaskScheduleScope | [UpdateTaskScheduleScopeRequest](#v1-UpdateTaskScheduleScopeRequest) | [UpdateTaskScheduleScopeResponse](#v1-UpdateTaskScheduleScopeResponse) | reconcile the full scope against a desired target_spec |
| ListTaskScheduleScopes | [ListTaskScheduleScopesRequest](#v1-ListTaskScheduleScopesRequest) | [ListTaskScheduleScopesResponse](#v1-ListTaskScheduleScopesResponse) | list all racks in a schedule&#39;s scope |
| CheckScheduleConflicts | [CheckScheduleConflictsRequest](#v1-CheckScheduleConflictsRequest) | [CheckScheduleConflictsResponse](#v1-CheckScheduleConflictsResponse) | advisory: returns existing schedules that may conflict with a proposed operation |
| CreateExpectedRack | [CreateExpectedRackRequest](#v1-CreateExpectedRackRequest) | [CreateExpectedRackResponse](#v1-CreateExpectedRackResponse) | Rack CRUD |
| GetRackInfoByID | [GetRackInfoByIDRequest](#v1-GetRackInfoByIDRequest) | [GetRackInfoResponse](#v1-GetRackInfoResponse) |  |
| GetRackInfoBySerial | [GetRackInfoBySerialRequest](#v1-GetRackInfoBySerialRequest) | [GetRackInfoResponse](#v1-GetRackInfoResponse) |  |
| GetListOfRacks | [GetListOfRacksRequest](#v1-GetListOfRacksRequest) | [GetListOfRacksResponse](#v1-GetListOfRacksResponse) |  |
| PatchRack | [PatchRackRequest](#v1-PatchRackRequest) | [PatchRackResponse](#v1-PatchRackResponse) |  |
| DeleteRack | [DeleteRackRequest](#v1-DeleteRackRequest) | [DeleteRackResponse](#v1-DeleteRackResponse) |  |
| PurgeRack | [PurgeRackRequest](#v1-PurgeRackRequest) | [PurgeRackResponse](#v1-PurgeRackResponse) |  |
| UpgradeFirmware | [UpgradeFirmwareRequest](#v1-UpgradeFirmwareRequest) | [SubmitTaskResponse](#v1-SubmitTaskResponse) | Rack operations |
| BringUpRack | [BringUpRackRequest](#v1-BringUpRackRequest) | [SubmitTaskResponse](#v1-SubmitTaskResponse) |  |
| IngestRack | [IngestRackRequest](#v1-IngestRackRequest) | [SubmitTaskResponse](#v1-SubmitTaskResponse) |  |
| PowerOnRack | [PowerOnRackRequest](#v1-PowerOnRackRequest) | [SubmitTaskResponse](#v1-SubmitTaskResponse) |  |
| PowerOffRack | [PowerOffRackRequest](#v1-PowerOffRackRequest) | [SubmitTaskResponse](#v1-SubmitTaskResponse) |  |
| PowerResetRack | [PowerResetRackRequest](#v1-PowerResetRackRequest) | [SubmitTaskResponse](#v1-SubmitTaskResponse) |  |
| GetComponentInfoByID | [GetComponentInfoByIDRequest](#v1-GetComponentInfoByIDRequest) | [GetComponentInfoResponse](#v1-GetComponentInfoResponse) | Component CRUD |
| GetComponentInfoBySerial | [GetComponentInfoBySerialRequest](#v1-GetComponentInfoBySerialRequest) | [GetComponentInfoResponse](#v1-GetComponentInfoResponse) |  |
| GetComponents | [GetComponentsRequest](#v1-GetComponentsRequest) | [GetComponentsResponse](#v1-GetComponentsResponse) |  |
| ValidateComponents | [ValidateComponentsRequest](#v1-ValidateComponentsRequest) | [ValidateComponentsResponse](#v1-ValidateComponentsResponse) |  |
| AddComponent | [AddComponentRequest](#v1-AddComponentRequest) | [AddComponentResponse](#v1-AddComponentResponse) |  |
| PatchComponent | [PatchComponentRequest](#v1-PatchComponentRequest) | [PatchComponentResponse](#v1-PatchComponentResponse) |  |
| DeleteComponent | [DeleteComponentRequest](#v1-DeleteComponentRequest) | [DeleteComponentResponse](#v1-DeleteComponentResponse) |  |
| PurgeComponent | [PurgeComponentRequest](#v1-PurgeComponentRequest) | [PurgeComponentResponse](#v1-PurgeComponentResponse) |  |
| CreateNVLDomain | [CreateNVLDomainRequest](#v1-CreateNVLDomainRequest) | [CreateNVLDomainResponse](#v1-CreateNVLDomainResponse) | NVL Domain |
| AttachRacksToNVLDomain | [AttachRacksToNVLDomainRequest](#v1-AttachRacksToNVLDomainRequest) | [.google.protobuf.Empty](#google-protobuf-Empty) |  |
| DetachRacksFromNVLDomain | [DetachRacksFromNVLDomainRequest](#v1-DetachRacksFromNVLDomainRequest) | [.google.protobuf.Empty](#google-protobuf-Empty) |  |
| GetListOfNVLDomains | [GetListOfNVLDomainsRequest](#v1-GetListOfNVLDomainsRequest) | [GetListOfNVLDomainsResponse](#v1-GetListOfNVLDomainsResponse) |  |
| GetRacksForNVLDomain | [GetRacksForNVLDomainRequest](#v1-GetRacksForNVLDomainRequest) | [GetRacksForNVLDomainResponse](#v1-GetRacksForNVLDomainResponse) |  |
| ListTasks | [ListTasksRequest](#v1-ListTasksRequest) | [ListTasksResponse](#v1-ListTasksResponse) | Tasks |
| GetTasksByIDs | [GetTasksByIDsRequest](#v1-GetTasksByIDsRequest) | [GetTasksByIDsResponse](#v1-GetTasksByIDsResponse) |  |
| CancelTask | [CancelTaskRequest](#v1-CancelTaskRequest) | [CancelTaskResponse](#v1-CancelTaskResponse) |  |
| CreateOperationRule | [CreateOperationRuleRequest](#v1-CreateOperationRuleRequest) | [CreateOperationRuleResponse](#v1-CreateOperationRuleResponse) | Operation rules |
| UpdateOperationRule | [UpdateOperationRuleRequest](#v1-UpdateOperationRuleRequest) | [.google.protobuf.Empty](#google-protobuf-Empty) |  |
| DeleteOperationRule | [DeleteOperationRuleRequest](#v1-DeleteOperationRuleRequest) | [.google.protobuf.Empty](#google-protobuf-Empty) |  |
| GetOperationRule | [GetOperationRuleRequest](#v1-GetOperationRuleRequest) | [OperationRule](#v1-OperationRule) |  |
| ListOperationRules | [ListOperationRulesRequest](#v1-ListOperationRulesRequest) | [ListOperationRulesResponse](#v1-ListOperationRulesResponse) |  |
| SetRuleAsDefault | [SetRuleAsDefaultRequest](#v1-SetRuleAsDefaultRequest) | [.google.protobuf.Empty](#google-protobuf-Empty) |  |
| AssociateRuleWithRack | [AssociateRuleWithRackRequest](#v1-AssociateRuleWithRackRequest) | [.google.protobuf.Empty](#google-protobuf-Empty) | Rack-rule associations |
| DisassociateRuleFromRack | [DisassociateRuleFromRackRequest](#v1-DisassociateRuleFromRackRequest) | [.google.protobuf.Empty](#google-protobuf-Empty) |  |
| GetRackRuleAssociation | [GetRackRuleAssociationRequest](#v1-GetRackRuleAssociationRequest) | [GetRackRuleAssociationResponse](#v1-GetRackRuleAssociationResponse) |  |
| ListRackRuleAssociations | [ListRackRuleAssociationsRequest](#v1-ListRackRuleAssociationsRequest) | [ListRackRuleAssociationsResponse](#v1-ListRackRuleAssociationsResponse) |  |

 



## Scalar Value Types

| .proto Type | Notes | C++ | Java | Python | Go | C# | PHP | Ruby |
| ----------- | ----- | --- | ---- | ------ | -- | -- | --- | ---- |
| <a name="double" /> double |  | double | double | float | float64 | double | float | Float |
| <a name="float" /> float |  | float | float | float | float32 | float | float | Float |
| <a name="int32" /> int32 | Uses variable-length encoding. Inefficient for encoding negative numbers – if your field is likely to have negative values, use sint32 instead. | int32 | int | int | int32 | int | integer | Bignum or Fixnum (as required) |
| <a name="int64" /> int64 | Uses variable-length encoding. Inefficient for encoding negative numbers – if your field is likely to have negative values, use sint64 instead. | int64 | long | int/long | int64 | long | integer/string | Bignum |
| <a name="uint32" /> uint32 | Uses variable-length encoding. | uint32 | int | int/long | uint32 | uint | integer | Bignum or Fixnum (as required) |
| <a name="uint64" /> uint64 | Uses variable-length encoding. | uint64 | long | int/long | uint64 | ulong | integer/string | Bignum or Fixnum (as required) |
| <a name="sint32" /> sint32 | Uses variable-length encoding. Signed int value. These more efficiently encode negative numbers than regular int32s. | int32 | int | int | int32 | int | integer | Bignum or Fixnum (as required) |
| <a name="sint64" /> sint64 | Uses variable-length encoding. Signed int value. These more efficiently encode negative numbers than regular int64s. | int64 | long | int/long | int64 | long | integer/string | Bignum |
| <a name="fixed32" /> fixed32 | Always four bytes. More efficient than uint32 if values are often greater than 2^28. | uint32 | int | int | uint32 | uint | integer | Bignum or Fixnum (as required) |
| <a name="fixed64" /> fixed64 | Always eight bytes. More efficient than uint64 if values are often greater than 2^56. | uint64 | long | int/long | uint64 | ulong | integer/string | Bignum |
| <a name="sfixed32" /> sfixed32 | Always four bytes. | int32 | int | int | int32 | int | integer | Bignum or Fixnum (as required) |
| <a name="sfixed64" /> sfixed64 | Always eight bytes. | int64 | long | int/long | int64 | long | integer/string | Bignum |
| <a name="bool" /> bool |  | bool | boolean | boolean | bool | bool | boolean | TrueClass/FalseClass |
| <a name="string" /> string | A string must always contain UTF-8 encoded or 7-bit ASCII text. | string | String | str/unicode | string | string | string | String (UTF-8) |
| <a name="bytes" /> bytes | May contain any arbitrary sequence of bytes. | string | ByteString | str | []byte | ByteString | string | String (ASCII-8BIT) |

