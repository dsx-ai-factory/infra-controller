# Protocol Documentation
<a name="top"></a>

## Table of Contents

- [flow.proto](#flow-proto)
    - [AddComponentRequest](#v1-AddComponentRequest)
    - [AddComponentResponse](#v1-AddComponentResponse)
    - [AddTaskScheduleScopeRequest](#v1-AddTaskScheduleScopeRequest)
    - [AddTaskScheduleScopeResponse](#v1-AddTaskScheduleScopeResponse)
    - [AdvanceOperationRunPhaseRequest](#v1-AdvanceOperationRunPhaseRequest)
    - [AssociateRuleWithRackRequest](#v1-AssociateRuleWithRackRequest)
    - [AttachRacksToNVLDomainRequest](#v1-AttachRacksToNVLDomainRequest)
    - [BMCInfo](#v1-BMCInfo)
    - [BringUpRackRequest](#v1-BringUpRackRequest)
    - [BuildInfo](#v1-BuildInfo)
    - [CancelOperationRunRequest](#v1-CancelOperationRunRequest)
    - [CancelTaskRequest](#v1-CancelTaskRequest)
    - [CancelTaskResponse](#v1-CancelTaskResponse)
    - [CheckScheduleConflictsRequest](#v1-CheckScheduleConflictsRequest)
    - [CheckScheduleConflictsResponse](#v1-CheckScheduleConflictsResponse)
    - [Component](#v1-Component)
    - [ComponentDiff](#v1-ComponentDiff)
    - [ComponentFilter](#v1-ComponentFilter)
    - [ComponentOperationStatus](#v1-ComponentOperationStatus)
    - [ComponentTarget](#v1-ComponentTarget)
    - [ComponentTargets](#v1-ComponentTargets)
    - [ComponentTypes](#v1-ComponentTypes)
    - [ComponentsByType](#v1-ComponentsByType)
    - [ComponentsForType](#v1-ComponentsForType)
    - [CountOperationRunPhases](#v1-CountOperationRunPhases)
    - [CreateEventRuleBindingRequest](#v1-CreateEventRuleBindingRequest)
    - [CreateEventRuleRequest](#v1-CreateEventRuleRequest)
    - [CreateExpectedRackRequest](#v1-CreateExpectedRackRequest)
    - [CreateExpectedRackResponse](#v1-CreateExpectedRackResponse)
    - [CreateNVLDomainRequest](#v1-CreateNVLDomainRequest)
    - [CreateNVLDomainResponse](#v1-CreateNVLDomainResponse)
    - [CreateOperationRuleRequest](#v1-CreateOperationRuleRequest)
    - [CreateOperationRuleResponse](#v1-CreateOperationRuleResponse)
    - [CreateOperationRunRequest](#v1-CreateOperationRunRequest)
    - [CreateOperationRunResponse](#v1-CreateOperationRunResponse)
    - [CreateTaskScheduleRequest](#v1-CreateTaskScheduleRequest)
    - [DecommissionRackRequest](#v1-DecommissionRackRequest)
    - [DeleteComponentRequest](#v1-DeleteComponentRequest)
    - [DeleteComponentResponse](#v1-DeleteComponentResponse)
    - [DeleteEventRuleBindingRequest](#v1-DeleteEventRuleBindingRequest)
    - [DeleteEventRuleRequest](#v1-DeleteEventRuleRequest)
    - [DeleteOperationRuleRequest](#v1-DeleteOperationRuleRequest)
    - [DeleteRackRequest](#v1-DeleteRackRequest)
    - [DeleteRackResponse](#v1-DeleteRackResponse)
    - [DeleteTaskScheduleRequest](#v1-DeleteTaskScheduleRequest)
    - [DetachRacksFromNVLDomainRequest](#v1-DetachRacksFromNVLDomainRequest)
    - [DeviceInfo](#v1-DeviceInfo)
    - [DeviceSerialInfo](#v1-DeviceSerialInfo)
    - [DisableEventRuleRequest](#v1-DisableEventRuleRequest)
    - [DisassociateRuleFromRackRequest](#v1-DisassociateRuleFromRackRequest)
    - [EnableEventRuleRequest](#v1-EnableEventRuleRequest)
    - [EqualOperationRunPhases](#v1-EqualOperationRunPhases)
    - [EventRule](#v1-EventRule)
    - [EventRuleAction](#v1-EventRuleAction)
    - [EventRuleActionCondition](#v1-EventRuleActionCondition)
    - [EventRuleActionsUpdate](#v1-EventRuleActionsUpdate)
    - [EventRuleBinding](#v1-EventRuleBinding)
    - [EventRuleMetadataUpdate](#v1-EventRuleMetadataUpdate)
    - [EventRuleNoopAction](#v1-EventRuleNoopAction)
    - [EventRuleScope](#v1-EventRuleScope)
    - [EventRuleSendAlertAction](#v1-EventRuleSendAlertAction)
    - [EventRuleSubmitTaskAction](#v1-EventRuleSubmitTaskAction)
    - [ExternalRef](#v1-ExternalRef)
    - [FieldDiff](#v1-FieldDiff)
    - [Filter](#v1-Filter)
    - [FirmwareAuthenticationData](#v1-FirmwareAuthenticationData)
    - [FirmwareControlTaskOperation](#v1-FirmwareControlTaskOperation)
    - [GetComponentInfoByIDRequest](#v1-GetComponentInfoByIDRequest)
    - [GetComponentInfoBySerialRequest](#v1-GetComponentInfoBySerialRequest)
    - [GetComponentInfoResponse](#v1-GetComponentInfoResponse)
    - [GetComponentsRequest](#v1-GetComponentsRequest)
    - [GetComponentsResponse](#v1-GetComponentsResponse)
    - [GetEffectiveEventRuleRequest](#v1-GetEffectiveEventRuleRequest)
    - [GetEventRuleBindingRequest](#v1-GetEventRuleBindingRequest)
    - [GetEventRuleRequest](#v1-GetEventRuleRequest)
    - [GetListOfNVLDomainsRequest](#v1-GetListOfNVLDomainsRequest)
    - [GetListOfNVLDomainsResponse](#v1-GetListOfNVLDomainsResponse)
    - [GetListOfRacksRequest](#v1-GetListOfRacksRequest)
    - [GetListOfRacksResponse](#v1-GetListOfRacksResponse)
    - [GetOperationRuleRequest](#v1-GetOperationRuleRequest)
    - [GetOperationRunRequest](#v1-GetOperationRunRequest)
    - [GetOperationRunResponse](#v1-GetOperationRunResponse)
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
    - [ListEventRulesRequest](#v1-ListEventRulesRequest)
    - [ListEventRulesResponse](#v1-ListEventRulesResponse)
    - [ListOperationRulesRequest](#v1-ListOperationRulesRequest)
    - [ListOperationRulesResponse](#v1-ListOperationRulesResponse)
    - [ListOperationRunTargetsRequest](#v1-ListOperationRunTargetsRequest)
    - [ListOperationRunTargetsResponse](#v1-ListOperationRunTargetsResponse)
    - [ListOperationRunsRequest](#v1-ListOperationRunsRequest)
    - [ListOperationRunsResponse](#v1-ListOperationRunsResponse)
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
    - [NVLDomainTarget](#v1-NVLDomainTarget)
    - [NVLDomainTargets](#v1-NVLDomainTargets)
    - [OperationKind](#v1-OperationKind)
    - [OperationRule](#v1-OperationRule)
    - [OperationRun](#v1-OperationRun)
    - [OperationRunConfiguration](#v1-OperationRunConfiguration)
    - [OperationRunConflictPolicy](#v1-OperationRunConflictPolicy)
    - [OperationRunConflictRetryPolicy](#v1-OperationRunConflictRetryPolicy)
    - [OperationRunCountPhase](#v1-OperationRunCountPhase)
    - [OperationRunFailureCountGate](#v1-OperationRunFailureCountGate)
    - [OperationRunFailureRateGate](#v1-OperationRunFailureRateGate)
    - [OperationRunFilter](#v1-OperationRunFilter)
    - [OperationRunOperation](#v1-OperationRunOperation)
    - [OperationRunOptions](#v1-OperationRunOptions)
    - [OperationRunOrderingPolicy](#v1-OperationRunOrderingPolicy)
    - [OperationRunPercentagePhase](#v1-OperationRunPercentagePhase)
    - [OperationRunPhaseAdvancePolicy](#v1-OperationRunPhaseAdvancePolicy)
    - [OperationRunPhasePolicy](#v1-OperationRunPhasePolicy)
    - [OperationRunPhaseStats](#v1-OperationRunPhaseStats)
    - [OperationRunPhysicalLocationOrdering](#v1-OperationRunPhysicalLocationOrdering)
    - [OperationRunRandomOrdering](#v1-OperationRunRandomOrdering)
    - [OperationRunSafetyGate](#v1-OperationRunSafetyGate)
    - [OperationRunSafetyPolicy](#v1-OperationRunSafetyPolicy)
    - [OperationRunSelector](#v1-OperationRunSelector)
    - [OperationRunState](#v1-OperationRunState)
    - [OperationRunStateFilter](#v1-OperationRunStateFilter)
    - [OperationRunStats](#v1-OperationRunStats)
    - [OperationRunSummary](#v1-OperationRunSummary)
    - [OperationRunTarget](#v1-OperationRunTarget)
    - [OperationRunTargetOutcomeCounts](#v1-OperationRunTargetOutcomeCounts)
    - [OperationRunTargetScope](#v1-OperationRunTargetScope)
    - [OperationTargetSpec](#v1-OperationTargetSpec)
    - [OrderBy](#v1-OrderBy)
    - [Pagination](#v1-Pagination)
    - [PatchComponentRequest](#v1-PatchComponentRequest)
    - [PatchComponentResponse](#v1-PatchComponentResponse)
    - [PatchRackRequest](#v1-PatchRackRequest)
    - [PatchRackResponse](#v1-PatchRackResponse)
    - [PauseOperationRunRequest](#v1-PauseOperationRunRequest)
    - [PauseTaskScheduleRequest](#v1-PauseTaskScheduleRequest)
    - [PerComponentFirmwareAuthenticationData](#v1-PerComponentFirmwareAuthenticationData)
    - [PercentageOperationRunPhases](#v1-PercentageOperationRunPhases)
    - [PercentageSelector](#v1-PercentageSelector)
    - [PowerControlTaskOperation](#v1-PowerControlTaskOperation)
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
    - [ResumeOperationRunRequest](#v1-ResumeOperationRunRequest)
    - [ResumeTaskScheduleRequest](#v1-ResumeTaskScheduleRequest)
    - [ScheduleConfig](#v1-ScheduleConfig)
    - [ScheduleSpec](#v1-ScheduleSpec)
    - [ScheduledOperation](#v1-ScheduledOperation)
    - [SetRuleAsDefaultRequest](#v1-SetRuleAsDefaultRequest)
    - [StringQueryInfo](#v1-StringQueryInfo)
    - [SubmitTaskResponse](#v1-SubmitTaskResponse)
    - [Task](#v1-Task)
    - [TaskOperation](#v1-TaskOperation)
    - [TaskSchedule](#v1-TaskSchedule)
    - [TaskScheduleScope](#v1-TaskScheduleScope)
    - [TaskStats](#v1-TaskStats)
    - [TriggerTaskScheduleRequest](#v1-TriggerTaskScheduleRequest)
    - [UUID](#v1-UUID)
    - [UpdateEventRuleRequest](#v1-UpdateEventRuleRequest)
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
    - [EventRuleConflictStrategy](#v1-EventRuleConflictStrategy)
    - [EventRuleScopeType](#v1-EventRuleScopeType)
    - [EventRuleSeverity](#v1-EventRuleSeverity)
    - [EventRuleTargetStrategy](#v1-EventRuleTargetStrategy)
    - [FirmwareControlOperation](#v1-FirmwareControlOperation)
    - [LeakStatus](#v1-LeakStatus)
    - [OperationRunPhysicalLocationOrdering.Strategy](#v1-OperationRunPhysicalLocationOrdering-Strategy)
    - [OperationRunSafetyGateScope](#v1-OperationRunSafetyGateScope)
    - [OperationRunStatus](#v1-OperationRunStatus)
    - [OperationRunStatusReason](#v1-OperationRunStatusReason)
    - [OperationRunTargetPhaseScope](#v1-OperationRunTargetPhaseScope)
    - [OperationRunTargetStatus](#v1-OperationRunTargetStatus)
    - [OperationType](#v1-OperationType)
    - [OverlapPolicy](#v1-OverlapPolicy)
    - [Phase](#v1-Phase)
    - [PowerControlOp](#v1-PowerControlOp)
    - [PowerControlOperation](#v1-PowerControlOperation)
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
Supports rack or NVLink domain targeting (with an optional component-type
filter) and component targeting (specific components by UUID or external reference).
NVLink domain membership is resolved to rack scopes when this request is handled.
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






<a name="v1-AdvanceOperationRunPhaseRequest"></a>

### AdvanceOperationRunPhaseRequest



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| id | [UUID](#v1-UUID) |  |  |
| expected_phase_index | [int32](#int32) | optional | Optional guard. When set, the phase that would be opened must match. |






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
| target_spec | [OperationTargetSpec](#v1-OperationTargetSpec) |  | Target racks, NVLink domains, or components for bring-up |
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






<a name="v1-CancelOperationRunRequest"></a>

### CancelOperationRunRequest



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| id | [UUID](#v1-UUID) |  |  |
| reason | [string](#string) |  |  |






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
| nvl_domain_id | [UUID](#v1-UUID) |  | NVLink Domain containing this component&#39;s rack; omitted when unassigned |
| task_stats | [TaskStats](#v1-TaskStats) |  | Active Tasks that explicitly target this component. |
| rack_external_id | [string](#string) |  |  |






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
| component_mac_address | [string](#string) |  | BMC MAC address identifying a missing expected component |






<a name="v1-ComponentFilter"></a>

### ComponentFilter
ComponentFilter is a reusable unresolved component filter. It describes
selection criteria, not the concrete components selected after planning.


| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| types | [ComponentTypes](#v1-ComponentTypes) |  |  |
| components | [ComponentTargets](#v1-ComponentTargets) |  |  |






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
| external | [ExternalRef](#v1-ExternalRef) |  |  |






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






<a name="v1-ComponentsByType"></a>

### ComponentsByType
ComponentsByType is the resolved component set selected for execution.


| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| groups | [ComponentsForType](#v1-ComponentsForType) | repeated |  |






<a name="v1-ComponentsForType"></a>

### ComponentsForType
ComponentsForType contains resolved component UUIDs of one component type.


| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| type | [ComponentType](#v1-ComponentType) |  |  |
| component_ids | [UUID](#v1-UUID) | repeated |  |






<a name="v1-CountOperationRunPhases"></a>

### CountOperationRunPhases



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| phases | [OperationRunCountPhase](#v1-OperationRunCountPhase) | repeated | Counts for phases before the generated final phase. The generated final phase covers the remaining candidate scope after all targets assigned by these defined count phases. |






<a name="v1-CreateEventRuleBindingRequest"></a>

### CreateEventRuleBindingRequest



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| rule_id | [UUID](#v1-UUID) |  |  |
| scope | [EventRuleScope](#v1-EventRuleScope) |  | Required. A rack scope ID must identify an existing inventory rack. |






<a name="v1-CreateEventRuleRequest"></a>

### CreateEventRuleRequest



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| name | [string](#string) |  |  |
| description | [string](#string) |  |  |
| event_type | [string](#string) |  | Required. Must be registered by Flow. Supported value: &#34;hardware.leak.detected&#34;. |
| actions | [EventRuleAction](#v1-EventRuleAction) | repeated |  |






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






<a name="v1-CreateOperationRunRequest"></a>

### CreateOperationRunRequest
CreateOperationRunRequest creates a durable rollout over a selected set of
rack execution targets. The operation request&#39;s target_spec and target_scope,
when present, define the candidate scope that selector is applied to. When
omitted, the service builds the candidate scope from all qualified racks that
are applicable to the operation.


| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| name | [string](#string) |  |  |
| description | [string](#string) |  |  |
| configuration | [OperationRunConfiguration](#v1-OperationRunConfiguration) |  | Required. Reusable rollout configuration for target selection, execution policy, and operation template. |






<a name="v1-CreateOperationRunResponse"></a>

### CreateOperationRunResponse



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| id | [UUID](#v1-UUID) |  |  |






<a name="v1-CreateTaskScheduleRequest"></a>

### CreateTaskScheduleRequest
CreateTaskScheduleRequest creates a new TaskSchedule.
The target_spec on the operation message defines the initial scope; it follows
the same targeting rules as AddTaskScheduleScope.
Use AddTaskScheduleScope / RemoveTaskScheduleScope to modify the scope after creation.


| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| schedule | [ScheduleConfig](#v1-ScheduleConfig) |  |  |
| operation | [ScheduledOperation](#v1-ScheduledOperation) |  |  |






<a name="v1-DecommissionRackRequest"></a>

### DecommissionRackRequest



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| target_spec | [OperationTargetSpec](#v1-OperationTargetSpec) |  | Target racks for decommissioning |
| description | [string](#string) |  | optional task description |
| queue_options | [QueueOptions](#v1-QueueOptions) | optional | optional queue policy overrides |
| rule_id | [UUID](#v1-UUID) | optional | optional: override rule resolution with a specific rule |






<a name="v1-DeleteComponentRequest"></a>

### DeleteComponentRequest
DeleteComponent - soft-delete a single component by UUID


| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| id | [UUID](#v1-UUID) |  | Required: component UUID to delete |






<a name="v1-DeleteComponentResponse"></a>

### DeleteComponentResponse







<a name="v1-DeleteEventRuleBindingRequest"></a>

### DeleteEventRuleBindingRequest



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| event_type | [string](#string) |  | Required. Identifies the event-family resolution slot to clear. Must be registered by Flow. Supported value: &#34;hardware.leak.detected&#34;. |
| scope | [EventRuleScope](#v1-EventRuleScope) |  | Required. Must exactly match the site or rack scope of the binding. |






<a name="v1-DeleteEventRuleRequest"></a>

### DeleteEventRuleRequest



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| rule_id | [UUID](#v1-UUID) |  |  |






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






<a name="v1-DisableEventRuleRequest"></a>

### DisableEventRuleRequest



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| rule_id | [UUID](#v1-UUID) |  |  |






<a name="v1-DisassociateRuleFromRackRequest"></a>

### DisassociateRuleFromRackRequest



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| rack_id | [UUID](#v1-UUID) |  |  |
| operation_type | [OperationType](#v1-OperationType) |  |  |
| operation_code | [string](#string) |  | Specific operation code |






<a name="v1-EnableEventRuleRequest"></a>

### EnableEventRuleRequest



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| rule_id | [UUID](#v1-UUID) |  |  |






<a name="v1-EqualOperationRunPhases"></a>

### EqualOperationRunPhases



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| phase_count | [int32](#int32) |  | Required. Example: 10 means ten roughly equal phases. |






<a name="v1-EventRule"></a>

### EventRule



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| id | [UUID](#v1-UUID) |  |  |
| read_only | [bool](#bool) |  |  |
| name | [string](#string) |  |  |
| description | [string](#string) |  |  |
| enabled | [bool](#bool) |  |  |
| event_type | [string](#string) |  |  |
| actions | [EventRuleAction](#v1-EventRuleAction) | repeated |  |
| created_at | [google.protobuf.Timestamp](https://protobuf.dev/reference/protobuf/google.protobuf/) |  |  |
| updated_at | [google.protobuf.Timestamp](https://protobuf.dev/reference/protobuf/google.protobuf/) |  |  |






<a name="v1-EventRuleAction"></a>

### EventRuleAction



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| name | [string](#string) |  |  |
| condition | [EventRuleActionCondition](#v1-EventRuleActionCondition) |  |  |
| submit_task | [EventRuleSubmitTaskAction](#v1-EventRuleSubmitTaskAction) |  |  |
| send_alert | [EventRuleSendAlertAction](#v1-EventRuleSendAlertAction) |  |  |
| noop | [EventRuleNoopAction](#v1-EventRuleNoopAction) |  |  |






<a name="v1-EventRuleActionCondition"></a>

### EventRuleActionCondition



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| severities | [EventRuleSeverity](#v1-EventRuleSeverity) | repeated | Optional. Matches when the event severity equals any listed value (OR). Omitted or empty imposes no severity constraint. When component_types is also non-empty, both field conditions must match (AND). |
| component_types | [ComponentType](#v1-ComponentType) | repeated | Optional. Matches a component resource whose type equals any listed value (OR). Omitted or empty imposes no component-type constraint. When severities is also non-empty, both field conditions must match (AND). |






<a name="v1-EventRuleActionsUpdate"></a>

### EventRuleActionsUpdate



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| actions | [EventRuleAction](#v1-EventRuleAction) | repeated |  |






<a name="v1-EventRuleBinding"></a>

### EventRuleBinding



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| id | [UUID](#v1-UUID) |  |  |
| rule_id | [UUID](#v1-UUID) |  |  |
| event_type | [string](#string) |  |  |
| scope | [EventRuleScope](#v1-EventRuleScope) |  |  |






<a name="v1-EventRuleMetadataUpdate"></a>

### EventRuleMetadataUpdate



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| name | [string](#string) |  |  |
| description | [string](#string) |  |  |






<a name="v1-EventRuleNoopAction"></a>

### EventRuleNoopAction



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| reason | [string](#string) |  |  |






<a name="v1-EventRuleScope"></a>

### EventRuleScope



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| type | [EventRuleScopeType](#v1-EventRuleScopeType) |  |  |
| id | [UUID](#v1-UUID) |  | Required for rack scope and omitted for site scope. |






<a name="v1-EventRuleSendAlertAction"></a>

### EventRuleSendAlertAction



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| severity | [EventRuleSeverity](#v1-EventRuleSeverity) |  |  |
| message | [string](#string) |  |  |






<a name="v1-EventRuleSubmitTaskAction"></a>

### EventRuleSubmitTaskAction



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| target_strategy | [EventRuleTargetStrategy](#v1-EventRuleTargetStrategy) |  |  |
| conflict_strategy | [EventRuleConflictStrategy](#v1-EventRuleConflictStrategy) |  |  |
| description | [string](#string) |  |  |
| operation | [TaskOperation](#v1-TaskOperation) |  | Required. The action must specify exactly one typed operation. Targets are derived from the event and target_strategy rather than embedded in this operation. |






<a name="v1-ExternalRef"></a>

### ExternalRef
ExternalRef identifies a component by its external identifier and optional type.


| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| type | [ComponentType](#v1-ComponentType) |  | UNKNOWN requires an unambiguous ID. |
| id | [string](#string) |  |  |






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






<a name="v1-FirmwareAuthenticationData"></a>

### FirmwareAuthenticationData
FirmwareAuthenticationData selects either one value shared by every target
or values scoped to supported firmware tray types.


| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| shared | [string](#string) |  |  |
| per_component | [PerComponentFirmwareAuthenticationData](#v1-PerComponentFirmwareAuthenticationData) |  |  |






<a name="v1-FirmwareControlTaskOperation"></a>

### FirmwareControlTaskOperation



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| operation | [FirmwareControlOperation](#v1-FirmwareControlOperation) |  | Required. UNSPECIFIED and unknown values are rejected. |
| target_version | [string](#string) | optional | Optional target firmware version. Omission leaves version selection to the firmware operation implementation. |
| start_time | [google.protobuf.Timestamp](https://protobuf.dev/reference/protobuf/google.protobuf/) | optional | Optional execution window. Supplied values are converted to whole Unix seconds. A value that converts to Unix second zero is rejected because zero represents omission. When both are set, end_time must be after start_time after conversion. |
| end_time | [google.protobuf.Timestamp](https://protobuf.dev/reference/protobuf/google.protobuf/) | optional |  |
| sub_targets | [string](#string) | repeated | Optional firmware sub-parts within each selected component. Empty means every firmware sub-part supported by that component. |
| override_readiness_check | [bool](#bool) |  | Bypasses the component readiness gate when the task executes. |






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
| target_spec | [OperationTargetSpec](#v1-OperationTargetSpec) | optional | Optional: target racks or NVLink domains with an optional type filter, or specific components. If not provided, queries all components. |
| filters | [Filter](#v1-Filter) | repeated | Filter conditions for component queries |
| pagination | [Pagination](#v1-Pagination) | optional |  |
| order_by | [OrderBy](#v1-OrderBy) | optional |  |






<a name="v1-GetComponentsResponse"></a>

### GetComponentsResponse



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| components | [Component](#v1-Component) | repeated |  |
| total | [int32](#int32) |  |  |






<a name="v1-GetEffectiveEventRuleRequest"></a>

### GetEffectiveEventRuleRequest



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| event_type | [string](#string) |  | Required. Must be registered by Flow. Supported value: &#34;hardware.leak.detected&#34;. |
| rack_id | [UUID](#v1-UUID) |  |  |
| component_id | [UUID](#v1-UUID) |  |  |






<a name="v1-GetEventRuleBindingRequest"></a>

### GetEventRuleBindingRequest



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| event_type | [string](#string) |  | Required. Must be registered by Flow. Supported value: &#34;hardware.leak.detected&#34;. |
| scope | [EventRuleScope](#v1-EventRuleScope) |  |  |






<a name="v1-GetEventRuleRequest"></a>

### GetEventRuleRequest



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| rule_id | [UUID](#v1-UUID) |  |  |






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






<a name="v1-GetOperationRunRequest"></a>

### GetOperationRunRequest



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| id | [UUID](#v1-UUID) |  |  |
| include_stats | [bool](#bool) |  | When true, Flow computes derived stats from operation_run_target rows and returns OperationRun.stats. |






<a name="v1-GetOperationRunResponse"></a>

### GetOperationRunResponse



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| operation_run | [OperationRun](#v1-OperationRun) |  |  |






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
| target_spec | [OperationTargetSpec](#v1-OperationTargetSpec) |  | Target racks, NVLink domains, or components for ingestion |
| filters | [Filter](#v1-Filter) | repeated | Filter conditions for component queries (e.g. by type, name) |
| description | [string](#string) |  | optional task description |
| rule_id | [UUID](#v1-UUID) | optional | optional: override rule resolution with a specific rule |






<a name="v1-ListEventRulesRequest"></a>

### ListEventRulesRequest



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| event_type | [string](#string) | optional | Optional. When set, must be registered by Flow. Supported value: &#34;hardware.leak.detected&#34;. Omit to return every supported event type. |
| enabled | [bool](#bool) | optional |  |
| pagination | [Pagination](#v1-Pagination) | optional | Optional. Omit for offset 0 and limit 100. When present, offset must be non-negative and limit must be greater than zero. |






<a name="v1-ListEventRulesResponse"></a>

### ListEventRulesResponse



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| rules | [EventRule](#v1-EventRule) | repeated |  |
| total | [int64](#int64) |  | Number of rules matching the filters before pagination. |






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






<a name="v1-ListOperationRunTargetsRequest"></a>

### ListOperationRunTargetsRequest
ListOperationRunTargetsRequest lists materialized rack execution targets for
one operation run. status UNKNOWN means no target-status filter is applied.
phase_scope UNKNOWN defaults to CURRENT_PHASE.


| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| operation_run_id | [UUID](#v1-UUID) |  |  |
| status | [OperationRunTargetStatus](#v1-OperationRunTargetStatus) |  |  |
| pagination | [Pagination](#v1-Pagination) | optional |  |
| phase_scope | [OperationRunTargetPhaseScope](#v1-OperationRunTargetPhaseScope) |  |  |






<a name="v1-ListOperationRunTargetsResponse"></a>

### ListOperationRunTargetsResponse



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| targets | [OperationRunTarget](#v1-OperationRunTarget) | repeated |  |
| total | [int32](#int32) |  |  |






<a name="v1-ListOperationRunsRequest"></a>

### ListOperationRunsRequest
ListOperationRunsRequest lists operation runs, newest first by default.


| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| filter | [OperationRunFilter](#v1-OperationRunFilter) |  |  |
| pagination | [Pagination](#v1-Pagination) | optional |  |






<a name="v1-ListOperationRunsResponse"></a>

### ListOperationRunsResponse



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| operation_runs | [OperationRunSummary](#v1-OperationRunSummary) | repeated |  |
| total | [int32](#int32) |  |  |






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
| rack_id | [UUID](#v1-UUID) | optional | Restrict by rack identifier. |
| active_only | [bool](#bool) |  | Restrict to non-terminal Tasks (Waiting, Pending, Running). |
| pagination | [Pagination](#v1-Pagination) | optional |  |
| component_id | [UUID](#v1-UUID) | optional | Restrict to Tasks that target this component identifier, regardless of component type. A rack_id plus component_id combination that references a component not on the given rack is not an error; it yields an empty result. |
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






<a name="v1-NVLDomainTarget"></a>

### NVLDomainTarget
NVLDomainTarget identifies an NVLink domain and optionally filters the
components selected from every rack currently belonging to that domain.


| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| id | [UUID](#v1-UUID) |  | NVLink domain UUID |
| name | [string](#string) |  | NVLink domain name |
| component_types | [ComponentType](#v1-ComponentType) | repeated | Optional: filter by component type. Omit (or send an empty list) to include all component types in the domain. |






<a name="v1-NVLDomainTargets"></a>

### NVLDomainTargets
NVLDomainTargets contains one or more NVLink domain targets.


| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| targets | [NVLDomainTarget](#v1-NVLDomainTarget) | repeated |  |






<a name="v1-OperationKind"></a>

### OperationKind



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| type | [OperationType](#v1-OperationType) |  |  |
| code | [string](#string) | optional |  |






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
| created_at | [google.protobuf.Timestamp](https://protobuf.dev/reference/protobuf/google.protobuf/) |  |  |
| updated_at | [google.protobuf.Timestamp](https://protobuf.dev/reference/protobuf/google.protobuf/) |  |  |






<a name="v1-OperationRun"></a>

### OperationRun



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| summary | [OperationRunSummary](#v1-OperationRunSummary) |  |  |
| configuration | [OperationRunConfiguration](#v1-OperationRunConfiguration) |  |  |
| stats | [OperationRunStats](#v1-OperationRunStats) |  | Present only when the request asks Flow to compute derived stats. |






<a name="v1-OperationRunConfiguration"></a>

### OperationRunConfiguration
OperationRunConfiguration is the create-time configuration that can be
returned on detailed OperationRun responses.


| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| selector | [OperationRunSelector](#v1-OperationRunSelector) |  |  |
| options | [OperationRunOptions](#v1-OperationRunOptions) |  |  |
| operation | [OperationRunOperation](#v1-OperationRunOperation) |  |  |






<a name="v1-OperationRunConflictPolicy"></a>

### OperationRunConflictPolicy



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| retry | [OperationRunConflictRetryPolicy](#v1-OperationRunConflictRetryPolicy) |  |  |






<a name="v1-OperationRunConflictRetryPolicy"></a>

### OperationRunConflictRetryPolicy



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| retry_timeout | [google.protobuf.Duration](https://protobuf.dev/reference/protobuf/google.protobuf/) |  | Optional. Missing values are filled from operation-specific defaults. |
| initial_retry_delay | [google.protobuf.Duration](https://protobuf.dev/reference/protobuf/google.protobuf/) |  |  |
| max_retry_delay | [google.protobuf.Duration](https://protobuf.dev/reference/protobuf/google.protobuf/) |  |  |






<a name="v1-OperationRunCountPhase"></a>

### OperationRunCountPhase



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| count | [int32](#int32) |  | Required. Must be greater than 0. |






<a name="v1-OperationRunFailureCountGate"></a>

### OperationRunFailureCountGate



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| scope | [OperationRunSafetyGateScope](#v1-OperationRunSafetyGateScope) |  | Optional. Default: CURRENT_PHASE. |
| failure_threshold_count | [int32](#int32) |  | Required. The dispatcher pauses when failed_targets reaches this count for the scope. |






<a name="v1-OperationRunFailureRateGate"></a>

### OperationRunFailureRateGate



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| scope | [OperationRunSafetyGateScope](#v1-OperationRunSafetyGateScope) |  | Optional. Default: CURRENT_PHASE. |
| failure_threshold_percent | [int32](#int32) |  | Required. Valid range: 1..100. The dispatcher pauses when failed_targets / planned_targets reaches this threshold for the scope. |






<a name="v1-OperationRunFilter"></a>

### OperationRunFilter



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| name | [StringQueryInfo](#v1-StringQueryInfo) |  | Optional name query. Name is a human label and is not unique. |
| states | [OperationRunStateFilter](#v1-OperationRunStateFilter) | repeated | Empty means all states. Each entry matches by AND-ing the fields set on that entry; the entries compose with OR. |
| operation_kinds | [OperationKind](#v1-OperationKind) | repeated | Empty means all operation kinds. A kind with no code matches all codes for the operation type. |






<a name="v1-OperationRunOperation"></a>

### OperationRunOperation



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| upgrade_firmware | [UpgradeFirmwareRequest](#v1-UpgradeFirmwareRequest) |  | In CreateOperationRun, target_spec is optional and defines candidate scope before selector is applied. In the existing UpgradeFirmware RPC, target_spec remains required and means &#34;run exactly on these targets&#34;. |
| target_scope | [OperationRunTargetScope](#v1-OperationRunTargetScope) |  | Optional exclusions applied after the embedded operation target_spec (or default qualified scope) is resolved. |






<a name="v1-OperationRunOptions"></a>

### OperationRunOptions



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| max_concurrent_targets | [int32](#int32) |  | Required. Maximum number of operation-run targets that may have active child tasks at the same time. |
| safety_policy | [OperationRunSafetyPolicy](#v1-OperationRunSafetyPolicy) |  | Required. Composable safety gates for the rollout. |
| conflict_policy | [OperationRunConflictPolicy](#v1-OperationRunConflictPolicy) |  | Optional. If omitted or partially specified, the service stores and returns the effective default policy for the operation type/code. |
| ordering_policy | [OperationRunOrderingPolicy](#v1-OperationRunOrderingPolicy) |  | Optional. If omitted, the service stores and returns the default random ordering policy with a generated seed. |
| phase_policy | [OperationRunPhasePolicy](#v1-OperationRunPhasePolicy) |  | Optional. If omitted, the run has one phase containing all selected rack execution targets. |






<a name="v1-OperationRunOrderingPolicy"></a>

### OperationRunOrderingPolicy



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| random | [OperationRunRandomOrdering](#v1-OperationRunRandomOrdering) |  |  |
| physical_location | [OperationRunPhysicalLocationOrdering](#v1-OperationRunPhysicalLocationOrdering) |  | Reserved for a later implementation. The first implementation keeps this API branch but rejects it as unsupported. |






<a name="v1-OperationRunPercentagePhase"></a>

### OperationRunPercentagePhase



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| percentage | [int32](#int32) |  | Required. Valid range: 1..100. Percentage phase values must sum to 100. |






<a name="v1-OperationRunPhaseAdvancePolicy"></a>

### OperationRunPhaseAdvancePolicy



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| auto_advance | [bool](#bool) |  | When false, a completed phase pauses with PHASE_GATE and waits for AdvanceOperationRunPhase. When true, the dispatcher advances to the next phase automatically as long as global safety gates are not tripped. |






<a name="v1-OperationRunPhasePolicy"></a>

### OperationRunPhasePolicy



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| equal | [EqualOperationRunPhases](#v1-EqualOperationRunPhases) |  |  |
| percentage | [PercentageOperationRunPhases](#v1-PercentageOperationRunPhases) |  |  |
| count | [CountOperationRunPhases](#v1-CountOperationRunPhases) |  |  |
| advance_policy | [OperationRunPhaseAdvancePolicy](#v1-OperationRunPhaseAdvancePolicy) |  | Optional. Default: manual phase advancement. |






<a name="v1-OperationRunPhaseStats"></a>

### OperationRunPhaseStats



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| phase_index | [int32](#int32) |  | For current_phase_stats, this is the current phase index. For cumulative_phase_stats, this is the latest included phase index. |
| selected_targets | [int32](#int32) |  |  |
| outcome_counts | [OperationRunTargetOutcomeCounts](#v1-OperationRunTargetOutcomeCounts) |  |  |






<a name="v1-OperationRunPhysicalLocationOrdering"></a>

### OperationRunPhysicalLocationOrdering



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| strategy | [OperationRunPhysicalLocationOrdering.Strategy](#v1-OperationRunPhysicalLocationOrdering-Strategy) |  |  |






<a name="v1-OperationRunRandomOrdering"></a>

### OperationRunRandomOrdering



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| seed | [string](#string) |  | Optional. If omitted, the service generates one and stores it. |






<a name="v1-OperationRunSafetyGate"></a>

### OperationRunSafetyGate



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| failure_rate | [OperationRunFailureRateGate](#v1-OperationRunFailureRateGate) |  |  |
| failure_count | [OperationRunFailureCountGate](#v1-OperationRunFailureCountGate) |  |  |






<a name="v1-OperationRunSafetyPolicy"></a>

### OperationRunSafetyPolicy



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| gates | [OperationRunSafetyGate](#v1-OperationRunSafetyGate) | repeated | Gates compose with OR: the dispatcher pauses the run when any gate crosses its configured threshold. |






<a name="v1-OperationRunSelector"></a>

### OperationRunSelector



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| percentage | [PercentageSelector](#v1-PercentageSelector) |  |  |






<a name="v1-OperationRunState"></a>

### OperationRunState



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| status | [OperationRunStatus](#v1-OperationRunStatus) |  |  |
| reason | [OperationRunStatusReason](#v1-OperationRunStatusReason) |  |  |






<a name="v1-OperationRunStateFilter"></a>

### OperationRunStateFilter



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| status | [OperationRunStatus](#v1-OperationRunStatus) | optional |  |
| reason | [OperationRunStatusReason](#v1-OperationRunStatusReason) | optional |  |






<a name="v1-OperationRunStats"></a>

### OperationRunStats



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| current_phase_stats | [OperationRunPhaseStats](#v1-OperationRunPhaseStats) |  |  |
| cumulative_phase_stats | [OperationRunPhaseStats](#v1-OperationRunPhaseStats) |  |  |






<a name="v1-OperationRunSummary"></a>

### OperationRunSummary
OperationRunSummary is the lightweight representation returned by
ListOperationRuns. It intentionally omits the full configuration and
target-derived phase stats; callers can use GetOperationRun for those details.


| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| id | [UUID](#v1-UUID) |  |  |
| name | [string](#string) |  |  |
| description | [string](#string) |  |  |
| operation_kind | [OperationKind](#v1-OperationKind) |  |  |
| state | [OperationRunState](#v1-OperationRunState) |  |  |
| status_message | [string](#string) |  |  |
| total_phases | [int32](#int32) |  |  |
| created_at | [google.protobuf.Timestamp](https://protobuf.dev/reference/protobuf/google.protobuf/) |  |  |
| updated_at | [google.protobuf.Timestamp](https://protobuf.dev/reference/protobuf/google.protobuf/) |  |  |
| started_at | [google.protobuf.Timestamp](https://protobuf.dev/reference/protobuf/google.protobuf/) | optional |  |
| finished_at | [google.protobuf.Timestamp](https://protobuf.dev/reference/protobuf/google.protobuf/) | optional |  |






<a name="v1-OperationRunTarget"></a>

### OperationRunTarget



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| id | [UUID](#v1-UUID) |  |  |
| operation_run_id | [UUID](#v1-UUID) |  |  |
| rack_id | [UUID](#v1-UUID) |  |  |
| sequence_index | [int32](#int32) |  |  |
| phase_index | [int32](#int32) |  |  |
| task_id | [UUID](#v1-UUID) |  | absent until a child task has been submitted |
| status | [OperationRunTargetStatus](#v1-OperationRunTargetStatus) |  |  |
| message | [string](#string) |  |  |
| components_by_type | [ComponentsByType](#v1-ComponentsByType) |  |  |
| created_at | [google.protobuf.Timestamp](https://protobuf.dev/reference/protobuf/google.protobuf/) |  |  |
| updated_at | [google.protobuf.Timestamp](https://protobuf.dev/reference/protobuf/google.protobuf/) |  |  |
| rack_external_id | [string](#string) |  |  |






<a name="v1-OperationRunTargetOutcomeCounts"></a>

### OperationRunTargetOutcomeCounts



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| completed | [int32](#int32) |  |  |
| failed | [int32](#int32) |  |  |
| terminated | [int32](#int32) |  |  |
| skipped | [int32](#int32) |  |  |






<a name="v1-OperationRunTargetScope"></a>

### OperationRunTargetScope



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| exclude_operation_run_ids | [UUID](#v1-UUID) | repeated | Prior operation runs whose materialized rack execution targets should be excluded from this run&#39;s candidate scope. The base scope is the embedded operation target_spec when present; otherwise it is the default qualified/applicable scope. |
| default_scope_component_filter | [ComponentFilter](#v1-ComponentFilter) |  | Restricts components when the embedded operation target_spec is omitted and the planner uses the default qualified/applicable rack scope. Omit to target all applicable components in the default scope. This must not be set together with an explicit operation target_spec. |






<a name="v1-OperationTargetSpec"></a>

### OperationTargetSpec
OperationTargetSpec contains targets for an operation.
Supports rack-level or NVLink-domain targeting (with optional type filtering),
or component-level targeting (by UUID or external reference), but not more
than one target kind at a time.


| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| racks | [RackTargets](#v1-RackTargets) |  |  |
| components | [ComponentTargets](#v1-ComponentTargets) |  |  |
| nvl_domains | [NVLDomainTargets](#v1-NVLDomainTargets) |  |  |






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






<a name="v1-PauseOperationRunRequest"></a>

### PauseOperationRunRequest



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| id | [UUID](#v1-UUID) |  |  |






<a name="v1-PauseTaskScheduleRequest"></a>

### PauseTaskScheduleRequest
PauseTaskScheduleRequest disables a TaskSchedule without deleting it.
The schedule will not fire until resumed. Has no effect if already paused.
Returns an error for a one-time schedule that has already fired.


| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| id | [UUID](#v1-UUID) |  |  |






<a name="v1-PerComponentFirmwareAuthenticationData"></a>

### PerComponentFirmwareAuthenticationData
PerComponentFirmwareAuthenticationData carries independent authentication
data for each supported firmware tray type. An omitted field means that tray
type receives no authentication data.


| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| compute | [string](#string) | optional |  |
| nvswitch | [string](#string) | optional |  |
| powershelf | [string](#string) | optional |  |






<a name="v1-PercentageOperationRunPhases"></a>

### PercentageOperationRunPhases



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| phases | [OperationRunPercentagePhase](#v1-OperationRunPercentagePhase) | repeated |  |






<a name="v1-PercentageSelector"></a>

### PercentageSelector



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| percentage | [int32](#int32) |  | Required. Valid range: 1..100. |
| seed | [string](#string) |  | Optional. If omitted, the service generates one and stores it so the selected cohort is deterministic and auditable. |






<a name="v1-PowerControlTaskOperation"></a>

### PowerControlTaskOperation



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| operation | [PowerControlOperation](#v1-PowerControlOperation) |  | Required. UNSPECIFIED and unknown values are rejected. |
| override_readiness_check | [bool](#bool) |  | Bypasses the component readiness gate when the task executes. |






<a name="v1-PowerOffRackRequest"></a>

### PowerOffRackRequest



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| target_spec | [OperationTargetSpec](#v1-OperationTargetSpec) |  | Target racks or NVLink domains with an optional type filter, or specific components |
| forced | [bool](#bool) |  |  |
| description | [string](#string) |  | optional task description |
| queue_options | [QueueOptions](#v1-QueueOptions) | optional |  |
| rule_id | [UUID](#v1-UUID) | optional | optional: override rule resolution with a specific rule |
| override_readiness_check | [bool](#bool) |  | When true, proceed with the power-off even if one or more target components (or, for rack-scoped components, any host on the owning rack) are reported as not ready for the operation by their persisted ComponentOperationStatus. Intended for operator-supervised maintenance where tenant impact has been acknowledged out-of-band; the bypass is recorded in the server log. |






<a name="v1-PowerOnRackRequest"></a>

### PowerOnRackRequest



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| target_spec | [OperationTargetSpec](#v1-OperationTargetSpec) |  | Target racks or NVLink domains with an optional type filter, or specific components |
| description | [string](#string) |  | optional task description |
| queue_options | [QueueOptions](#v1-QueueOptions) | optional |  |
| rule_id | [UUID](#v1-UUID) | optional | optional: override rule resolution with a specific rule |
| override_readiness_check | [bool](#bool) |  | When true, proceed with the power-on even if one or more target components (or, for rack-scoped components, any host on the owning rack) are reported as not ready for the operation by their persisted ComponentOperationStatus. Intended for operator-supervised maintenance where tenant impact has been acknowledged out-of-band; the bypass is recorded in the server log. |






<a name="v1-PowerResetRackRequest"></a>

### PowerResetRackRequest



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| target_spec | [OperationTargetSpec](#v1-OperationTargetSpec) |  | Target racks or NVLink domains with an optional type filter, or specific components |
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
| nvl_domain_ids | [UUID](#v1-UUID) | repeated | NVLink Domains containing this rack; empty when unassigned |
| task_stats | [TaskStats](#v1-TaskStats) |  | All active Tasks on this rack, including component-scoped Tasks. |
| external_id | [string](#string) |  |  |






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
| created_at | [google.protobuf.Timestamp](https://protobuf.dev/reference/protobuf/google.protobuf/) |  |  |
| updated_at | [google.protobuf.Timestamp](https://protobuf.dev/reference/protobuf/google.protobuf/) |  |  |






<a name="v1-RackTarget"></a>

### RackTarget
RackTarget identifies a rack and optionally filters by component type.
To target specific components, use the component-level APIs instead.


| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| id | [UUID](#v1-UUID) |  | Flow rack UUID |
| name | [string](#string) |  | Rack name |
| external_id | [string](#string) |  |  |
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






<a name="v1-ResumeOperationRunRequest"></a>

### ResumeOperationRunRequest



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| id | [UUID](#v1-UUID) |  |  |






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
| queue_expires_at | [google.protobuf.Timestamp](https://protobuf.dev/reference/protobuf/google.protobuf/) | optional | queue_expires_at is set only for waiting tasks; absent for all other statuses. |
| created_at | [google.protobuf.Timestamp](https://protobuf.dev/reference/protobuf/google.protobuf/) |  |  |
| finished_at | [google.protobuf.Timestamp](https://protobuf.dev/reference/protobuf/google.protobuf/) | optional |  |
| applied_rule_id | [UUID](#v1-UUID) | optional |  |
| updated_at | [google.protobuf.Timestamp](https://protobuf.dev/reference/protobuf/google.protobuf/) |  |  |
| started_at | [google.protobuf.Timestamp](https://protobuf.dev/reference/protobuf/google.protobuf/) | optional |  |
| report | [string](#string) |  | report is a versioned JSON document with structured execution progress. |






<a name="v1-TaskOperation"></a>

### TaskOperation
TaskOperation is the typed, target-independent operation definition used by
new APIs. Existing operation APIs retain their established request messages.


| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| power_control | [PowerControlTaskOperation](#v1-PowerControlTaskOperation) |  |  |
| firmware_control | [FirmwareControlTaskOperation](#v1-FirmwareControlTaskOperation) |  |  |






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
| next_run_at | [google.protobuf.Timestamp](https://protobuf.dev/reference/protobuf/google.protobuf/) | optional | absent for disabled or fully-fired one-time schedules |
| last_run_at | [google.protobuf.Timestamp](https://protobuf.dev/reference/protobuf/google.protobuf/) | optional | absent if the schedule has never fired |
| created_at | [google.protobuf.Timestamp](https://protobuf.dev/reference/protobuf/google.protobuf/) |  |  |
| updated_at | [google.protobuf.Timestamp](https://protobuf.dev/reference/protobuf/google.protobuf/) |  |  |
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
| created_at | [google.protobuf.Timestamp](https://protobuf.dev/reference/protobuf/google.protobuf/) |  |  |






<a name="v1-TaskStats"></a>

### TaskStats
TaskStats counts non-terminal Tasks currently associated with an inventory resource.


| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| waiting_task_count | [uint32](#uint32) |  |  |
| pending_task_count | [uint32](#uint32) |  |  |
| running_task_count | [uint32](#uint32) |  |  |






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






<a name="v1-UpdateEventRuleRequest"></a>

### UpdateEventRuleRequest



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| rule_id | [UUID](#v1-UUID) |  |  |
| metadata | [EventRuleMetadataUpdate](#v1-EventRuleMetadataUpdate) |  |  |
| actions | [EventRuleActionsUpdate](#v1-EventRuleActionsUpdate) |  |  |






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
| update_mask | [google.protobuf.FieldMask](https://protobuf.dev/reference/protobuf/google.protobuf/) |  |  |






<a name="v1-UpdateTaskScheduleScopeRequest"></a>

### UpdateTaskScheduleScopeRequest
UpdateTaskScheduleScopeRequest reconciles the schedule&#39;s scope against the
desired target_spec: racks present in desired_scope but not in the current scope
are added; racks present in the current scope but absent from desired_scope are
removed; racks present in both have their component_filter updated if changed.
For NVLink domain and component targets the server resolves rack membership automatically.


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
| start_time | [google.protobuf.Timestamp](https://protobuf.dev/reference/protobuf/google.protobuf/) | optional | optional: scheduled start time |
| end_time | [google.protobuf.Timestamp](https://protobuf.dev/reference/protobuf/google.protobuf/) | optional | optional: scheduled end time |
| description | [string](#string) |  | optional: task description |
| queue_options | [QueueOptions](#v1-QueueOptions) | optional |  |
| rule_id | [UUID](#v1-UUID) | optional | optional: override rule resolution with a specific rule |
| sub_targets | [string](#string) | repeated | Optional subset of firmware sub-parts to update within each tray selected by target_spec, e.g. [&#34;bmc&#34;, &#34;nvos&#34;] for switch trays or [&#34;psu&#34;] for powershelf trays. Named &#34;sub_targets&#34; (not &#34;components&#34;) to avoid colliding with OperationTargetSpec.components, which selects tray INSTANCES rather than sub-parts of a tray. Names are lowercase. Empty or omitted means update everything in the bundle (current default behavior). Unknown names are rejected by the downstream component manager. |
| override_readiness_check | [bool](#bool) |  | When true, proceed with the firmware update even if one or more target components (or, for rack-scoped components, any host on the owning rack) are reported as not ready for the operation by their persisted ComponentOperationStatus. The flag is intended for operator-supervised maintenance windows where the tenant impact has been acknowledged out-of-band; setting it bypasses the readiness gate that would otherwise block disruptive operations against tenanted hardware. The bypass is recorded in the server log. |
| authentication_data | [FirmwareAuthenticationData](#v1-FirmwareAuthenticationData) |  | Optional, write-only authentication data for firmware downloads. It is not supported for DPU-only updates or by the legacy NICo compute firmware controller. |






<a name="v1-ValidateComponentsRequest"></a>

### ValidateComponentsRequest



| Field | Type | Label | Description |
| ----- | ---- | ----- | ----------- |
| target_spec | [OperationTargetSpec](#v1-OperationTargetSpec) | optional | Optional: target racks or NVLink domains with an optional type filter, or specific components. If not provided, returns all diffs. |
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



<a name="v1-EventRuleConflictStrategy"></a>

### EventRuleConflictStrategy


| Name | Number | Description |
| ---- | ------ | ----------- |
| EVENT_RULE_CONFLICT_STRATEGY_UNSPECIFIED | 0 |  |
| EVENT_RULE_CONFLICT_STRATEGY_QUEUE | 1 |  |
| EVENT_RULE_CONFLICT_STRATEGY_REJECT | 2 |  |



<a name="v1-EventRuleScopeType"></a>

### EventRuleScopeType


| Name | Number | Description |
| ---- | ------ | ----------- |
| EVENT_RULE_SCOPE_TYPE_UNSPECIFIED | 0 |  |
| EVENT_RULE_SCOPE_TYPE_SITE | 1 |  |
| EVENT_RULE_SCOPE_TYPE_RACK | 2 |  |



<a name="v1-EventRuleSeverity"></a>

### EventRuleSeverity


| Name | Number | Description |
| ---- | ------ | ----------- |
| EVENT_RULE_SEVERITY_UNSPECIFIED | 0 |  |
| EVENT_RULE_SEVERITY_INFO | 1 |  |
| EVENT_RULE_SEVERITY_WARNING | 2 |  |
| EVENT_RULE_SEVERITY_CRITICAL | 3 |  |



<a name="v1-EventRuleTargetStrategy"></a>

### EventRuleTargetStrategy


| Name | Number | Description |
| ---- | ------ | ----------- |
| EVENT_RULE_TARGET_STRATEGY_UNSPECIFIED | 0 |  |
| EVENT_RULE_TARGET_STRATEGY_COMPONENT | 1 |  |
| EVENT_RULE_TARGET_STRATEGY_RACK | 2 |  |
| EVENT_RULE_TARGET_STRATEGY_AFFECTED_COMPONENTS | 3 |  |



<a name="v1-FirmwareControlOperation"></a>

### FirmwareControlOperation


| Name | Number | Description |
| ---- | ------ | ----------- |
| FIRMWARE_CONTROL_OPERATION_UNSPECIFIED | 0 |  |
| FIRMWARE_CONTROL_OPERATION_UPGRADE | 1 |  |
| FIRMWARE_CONTROL_OPERATION_DOWNGRADE | 2 |  |
| FIRMWARE_CONTROL_OPERATION_ROLLBACK | 3 |  |



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



<a name="v1-OperationRunPhysicalLocationOrdering-Strategy"></a>

### OperationRunPhysicalLocationOrdering.Strategy


| Name | Number | Description |
| ---- | ------ | ----------- |
| STRATEGY_UNKNOWN | 0 |  |
| STRATEGY_ROW_BY_ROW | 1 |  |
| STRATEGY_ONE_PER_ROW_ROUND_ROBIN | 2 |  |



<a name="v1-OperationRunSafetyGateScope"></a>

### OperationRunSafetyGateScope


| Name | Number | Description |
| ---- | ------ | ----------- |
| OPERATION_RUN_SAFETY_GATE_SCOPE_UNKNOWN | 0 |  |
| OPERATION_RUN_SAFETY_GATE_SCOPE_CURRENT_PHASE | 1 |  |
| OPERATION_RUN_SAFETY_GATE_SCOPE_CUMULATIVE_RUN | 2 |  |



<a name="v1-OperationRunStatus"></a>

### OperationRunStatus


| Name | Number | Description |
| ---- | ------ | ----------- |
| OPERATION_RUN_STATUS_UNKNOWN | 0 |  |
| OPERATION_RUN_STATUS_PENDING | 1 |  |
| OPERATION_RUN_STATUS_RUNNING | 2 |  |
| OPERATION_RUN_STATUS_PAUSED | 3 |  |
| OPERATION_RUN_STATUS_COMPLETED | 4 |  |
| OPERATION_RUN_STATUS_CANCELLED | 5 |  |
| OPERATION_RUN_STATUS_FAILED | 6 |  |
| OPERATION_RUN_STATUS_COMPLETED_WITH_FAILURES | 7 |  |



<a name="v1-OperationRunStatusReason"></a>

### OperationRunStatusReason


| Name | Number | Description |
| ---- | ------ | ----------- |
| OPERATION_RUN_STATUS_REASON_UNKNOWN | 0 |  |
| OPERATION_RUN_STATUS_REASON_NONE | 1 |  |
| OPERATION_RUN_STATUS_REASON_OPERATOR_PAUSED | 2 |  |
| OPERATION_RUN_STATUS_REASON_PHASE_GATE | 3 |  |
| OPERATION_RUN_STATUS_REASON_SAFETY_GATE | 4 |  |
| OPERATION_RUN_STATUS_REASON_CONFLICT_RETRY_TIMEOUT | 5 |  |



<a name="v1-OperationRunTargetPhaseScope"></a>

### OperationRunTargetPhaseScope


| Name | Number | Description |
| ---- | ------ | ----------- |
| OPERATION_RUN_TARGET_PHASE_SCOPE_UNKNOWN | 0 |  |
| OPERATION_RUN_TARGET_PHASE_SCOPE_CURRENT_PHASE | 1 | Default. Targets in the first materialized phase with non-terminal targets. |
| OPERATION_RUN_TARGET_PHASE_SCOPE_COMPLETED_PHASES | 2 | Targets in materialized phases before the current phase. If no current phase exists, every materialized phase is completed. |
| OPERATION_RUN_TARGET_PHASE_SCOPE_CURRENT_AND_COMPLETED_PHASES | 3 | All materialized targets through the current phase. If no current phase exists, this includes every materialized phase. |



<a name="v1-OperationRunTargetStatus"></a>

### OperationRunTargetStatus


| Name | Number | Description |
| ---- | ------ | ----------- |
| OPERATION_RUN_TARGET_STATUS_UNKNOWN | 0 |  |
| OPERATION_RUN_TARGET_STATUS_PENDING | 1 |  |
| OPERATION_RUN_TARGET_STATUS_BLOCKED | 2 |  |
| OPERATION_RUN_TARGET_STATUS_SUBMITTED | 3 |  |
| OPERATION_RUN_TARGET_STATUS_COMPLETED | 4 |  |
| OPERATION_RUN_TARGET_STATUS_FAILED | 5 |  |
| OPERATION_RUN_TARGET_STATUS_TERMINATED | 6 |  |
| OPERATION_RUN_TARGET_STATUS_SKIPPED | 7 |  |
| OPERATION_RUN_TARGET_STATUS_CLAIMED | 8 |  |



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



<a name="v1-PowerControlOperation"></a>

### PowerControlOperation


| Name | Number | Description |
| ---- | ------ | ----------- |
| POWER_CONTROL_OPERATION_UNSPECIFIED | 0 |  |
| POWER_CONTROL_OPERATION_POWER_ON | 1 |  |
| POWER_CONTROL_OPERATION_FORCE_POWER_ON | 2 |  |
| POWER_CONTROL_OPERATION_POWER_OFF | 3 |  |
| POWER_CONTROL_OPERATION_FORCE_POWER_OFF | 4 |  |
| POWER_CONTROL_OPERATION_RESTART | 5 |  |
| POWER_CONTROL_OPERATION_FORCE_RESTART | 6 |  |
| POWER_CONTROL_OPERATION_WARM_RESET | 7 |  |
| POWER_CONTROL_OPERATION_COLD_RESET | 8 |  |



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
| DeleteTaskSchedule | [DeleteTaskScheduleRequest](#v1-DeleteTaskScheduleRequest) | [.google.protobuf.Empty](https://protobuf.dev/reference/protobuf/google.protobuf/) |  |
| TriggerTaskSchedule | [TriggerTaskScheduleRequest](#v1-TriggerTaskScheduleRequest) | [SubmitTaskResponse](#v1-SubmitTaskResponse) |  |
| AddTaskScheduleScope | [AddTaskScheduleScopeRequest](#v1-AddTaskScheduleScopeRequest) | [AddTaskScheduleScopeResponse](#v1-AddTaskScheduleScopeResponse) | add one or more racks to a schedule&#39;s scope |
| RemoveTaskScheduleScope | [RemoveTaskScheduleScopeRequest](#v1-RemoveTaskScheduleScopeRequest) | [.google.protobuf.Empty](https://protobuf.dev/reference/protobuf/google.protobuf/) | remove a single rack from a schedule&#39;s scope by scope ID |
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
| DecommissionRack | [DecommissionRackRequest](#v1-DecommissionRackRequest) | [SubmitTaskResponse](#v1-SubmitTaskResponse) |  |
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
| AttachRacksToNVLDomain | [AttachRacksToNVLDomainRequest](#v1-AttachRacksToNVLDomainRequest) | [.google.protobuf.Empty](https://protobuf.dev/reference/protobuf/google.protobuf/) |  |
| DetachRacksFromNVLDomain | [DetachRacksFromNVLDomainRequest](#v1-DetachRacksFromNVLDomainRequest) | [.google.protobuf.Empty](https://protobuf.dev/reference/protobuf/google.protobuf/) |  |
| GetListOfNVLDomains | [GetListOfNVLDomainsRequest](#v1-GetListOfNVLDomainsRequest) | [GetListOfNVLDomainsResponse](#v1-GetListOfNVLDomainsResponse) |  |
| GetRacksForNVLDomain | [GetRacksForNVLDomainRequest](#v1-GetRacksForNVLDomainRequest) | [GetRacksForNVLDomainResponse](#v1-GetRacksForNVLDomainResponse) |  |
| ListTasks | [ListTasksRequest](#v1-ListTasksRequest) | [ListTasksResponse](#v1-ListTasksResponse) | Tasks |
| GetTasksByIDs | [GetTasksByIDsRequest](#v1-GetTasksByIDsRequest) | [GetTasksByIDsResponse](#v1-GetTasksByIDsResponse) |  |
| CancelTask | [CancelTaskRequest](#v1-CancelTaskRequest) | [CancelTaskResponse](#v1-CancelTaskResponse) |  |
| CreateOperationRule | [CreateOperationRuleRequest](#v1-CreateOperationRuleRequest) | [CreateOperationRuleResponse](#v1-CreateOperationRuleResponse) | Operation rules |
| UpdateOperationRule | [UpdateOperationRuleRequest](#v1-UpdateOperationRuleRequest) | [.google.protobuf.Empty](https://protobuf.dev/reference/protobuf/google.protobuf/) |  |
| DeleteOperationRule | [DeleteOperationRuleRequest](#v1-DeleteOperationRuleRequest) | [.google.protobuf.Empty](https://protobuf.dev/reference/protobuf/google.protobuf/) |  |
| GetOperationRule | [GetOperationRuleRequest](#v1-GetOperationRuleRequest) | [OperationRule](#v1-OperationRule) |  |
| ListOperationRules | [ListOperationRulesRequest](#v1-ListOperationRulesRequest) | [ListOperationRulesResponse](#v1-ListOperationRulesResponse) |  |
| SetRuleAsDefault | [SetRuleAsDefaultRequest](#v1-SetRuleAsDefaultRequest) | [.google.protobuf.Empty](https://protobuf.dev/reference/protobuf/google.protobuf/) |  |
| CreateEventRule | [CreateEventRuleRequest](#v1-CreateEventRuleRequest) | [EventRule](#v1-EventRule) | Creates a persisted event rule in the disabled state. The returned rule always has enabled set to false and does not participate in effective-rule selection. Callers may create its bindings while it is disabled, but must call EnableEventRule before it can override a site or built-in rule. |
| GetEventRule | [GetEventRuleRequest](#v1-GetEventRuleRequest) | [EventRule](#v1-EventRule) |  |
| GetEffectiveEventRule | [GetEffectiveEventRuleRequest](#v1-GetEffectiveEventRuleRequest) | [EventRule](#v1-EventRule) |  |
| ListEventRules | [ListEventRulesRequest](#v1-ListEventRulesRequest) | [ListEventRulesResponse](#v1-ListEventRulesResponse) | Returns matching persisted rules followed by built-in rules, with each group ordered by ascending rule ID. Filters are applied before pagination. When pagination is omitted, offset defaults to 0 and limit defaults to 100. |
| UpdateEventRule | [UpdateEventRuleRequest](#v1-UpdateEventRuleRequest) | [EventRule](#v1-EventRule) |  |
| EnableEventRule | [EnableEventRuleRequest](#v1-EnableEventRuleRequest) | [EventRule](#v1-EventRule) |  |
| DisableEventRule | [DisableEventRuleRequest](#v1-DisableEventRuleRequest) | [EventRule](#v1-EventRule) |  |
| DeleteEventRule | [DeleteEventRuleRequest](#v1-DeleteEventRuleRequest) | [.google.protobuf.Empty](https://protobuf.dev/reference/protobuf/google.protobuf/) |  |
| CreateEventRuleBinding | [CreateEventRuleBindingRequest](#v1-CreateEventRuleBindingRequest) | [EventRuleBinding](#v1-EventRuleBinding) | Event-rule bindings |
| GetEventRuleBinding | [GetEventRuleBindingRequest](#v1-GetEventRuleBindingRequest) | [EventRuleBinding](#v1-EventRuleBinding) |  |
| DeleteEventRuleBinding | [DeleteEventRuleBindingRequest](#v1-DeleteEventRuleBindingRequest) | [.google.protobuf.Empty](https://protobuf.dev/reference/protobuf/google.protobuf/) |  |
| AssociateRuleWithRack | [AssociateRuleWithRackRequest](#v1-AssociateRuleWithRackRequest) | [.google.protobuf.Empty](https://protobuf.dev/reference/protobuf/google.protobuf/) | Rack-rule associations |
| DisassociateRuleFromRack | [DisassociateRuleFromRackRequest](#v1-DisassociateRuleFromRackRequest) | [.google.protobuf.Empty](https://protobuf.dev/reference/protobuf/google.protobuf/) |  |
| GetRackRuleAssociation | [GetRackRuleAssociationRequest](#v1-GetRackRuleAssociationRequest) | [GetRackRuleAssociationResponse](#v1-GetRackRuleAssociationResponse) |  |
| ListRackRuleAssociations | [ListRackRuleAssociationsRequest](#v1-ListRackRuleAssociationsRequest) | [ListRackRuleAssociationsResponse](#v1-ListRackRuleAssociationsResponse) |  |
| CreateOperationRun | [CreateOperationRunRequest](#v1-CreateOperationRunRequest) | [CreateOperationRunResponse](#v1-CreateOperationRunResponse) | Operation runs |
| GetOperationRun | [GetOperationRunRequest](#v1-GetOperationRunRequest) | [GetOperationRunResponse](#v1-GetOperationRunResponse) |  |
| ListOperationRuns | [ListOperationRunsRequest](#v1-ListOperationRunsRequest) | [ListOperationRunsResponse](#v1-ListOperationRunsResponse) |  |
| ListOperationRunTargets | [ListOperationRunTargetsRequest](#v1-ListOperationRunTargetsRequest) | [ListOperationRunTargetsResponse](#v1-ListOperationRunTargetsResponse) |  |
| PauseOperationRun | [PauseOperationRunRequest](#v1-PauseOperationRunRequest) | [OperationRun](#v1-OperationRun) |  |
| ResumeOperationRun | [ResumeOperationRunRequest](#v1-ResumeOperationRunRequest) | [OperationRun](#v1-OperationRun) |  |
| AdvanceOperationRunPhase | [AdvanceOperationRunPhaseRequest](#v1-AdvanceOperationRunPhaseRequest) | [OperationRun](#v1-OperationRun) |  |
| CancelOperationRun | [CancelOperationRunRequest](#v1-CancelOperationRunRequest) | [OperationRun](#v1-OperationRun) |  |





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

