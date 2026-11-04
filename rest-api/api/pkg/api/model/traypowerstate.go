// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"time"

	"google.golang.org/protobuf/types/known/timestamppb"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
)

// APIPowerState names observed hardware states; a nil field means no observation.
type APIPowerState string

// APIPowerSource identifies the hardware reader behind an observation.
type APIPowerSource string

// APIPowerReadError is a bounded failure category, without dependency text.
type APIPowerReadError string

// APIPowerPolicyState names a stored host power policy, including disabled management.
type APIPowerPolicyState string

// APIPowerPolicyStatus distinguishes available, absent and inapplicable policy.
type APIPowerPolicyStatus string

// APIPowerActionStatus identifies active or pending Core maintenance.
type APIPowerActionStatus string

// APIPowerActionOperation names the saved Core maintenance operation.
type APIPowerActionOperation string

// APIPowerTaskOperation names the power operation requested through Flow.
type APIPowerTaskOperation string

// APIPowerTaskRelevance explains why a task appears for the requested tray.
type APIPowerTaskRelevance string

// APITaskType categorizes an active conflict or earlier waiting task, including non-power work.
type APITaskType string

var powerStateNames = map[corev1.ComponentPowerState]APIPowerState{
	corev1.ComponentPowerState_COMPONENT_POWER_STATE_UNKNOWN:      "Unknown",
	corev1.ComponentPowerState_COMPONENT_POWER_STATE_ON:           "On",
	corev1.ComponentPowerState_COMPONENT_POWER_STATE_OFF:          "Off",
	corev1.ComponentPowerState_COMPONENT_POWER_STATE_POWERING_ON:  "PoweringOn",
	corev1.ComponentPowerState_COMPONENT_POWER_STATE_POWERING_OFF: "PoweringOff",
	corev1.ComponentPowerState_COMPONENT_POWER_STATE_PAUSED:       "Paused",
	corev1.ComponentPowerState_COMPONENT_POWER_STATE_RESET:        "Reset",
}

var powerPolicyStateNames = map[corev1.PowerState]APIPowerPolicyState{
	corev1.PowerState_On:                   "On",
	corev1.PowerState_Off:                  "Off",
	corev1.PowerState_PowerManagerDisabled: "PowerManagerDisabled",
}

var powerSourceNames = map[corev1.ComponentPowerSource]APIPowerSource{
	corev1.ComponentPowerSource_COMPONENT_POWER_SOURCE_REDFISH: "Redfish",
	corev1.ComponentPowerSource_COMPONENT_POWER_SOURCE_RMS:     "RMS",
}

var powerReadErrorNames = map[flowv1.PowerDetailsReadError]APIPowerReadError{
	flowv1.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_NOT_LINKED:        "NotLinked",
	flowv1.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_NOT_CONFIGURED:    "NotConfigured",
	flowv1.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_UNSUPPORTED:       "Unsupported",
	flowv1.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_DEADLINE_EXCEEDED: "DeadlineExceeded",
	flowv1.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_READ_FAILED:       "ReadFailed",
	flowv1.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_INVALID_RESPONSE:  "InvalidResponse",
	flowv1.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_LIMIT_EXCEEDED:    "LimitExceeded",
}

var corePowerErrorNames = map[corev1.ComponentPowerErrorCode]APIPowerReadError{
	corev1.ComponentPowerErrorCode_COMPONENT_POWER_ERROR_CODE_NOT_FOUND:               "NotFound",
	corev1.ComponentPowerErrorCode_COMPONENT_POWER_ERROR_CODE_DISABLED:                "Disabled",
	corev1.ComponentPowerErrorCode_COMPONENT_POWER_ERROR_CODE_UNSUPPORTED:             "Unsupported",
	corev1.ComponentPowerErrorCode_COMPONENT_POWER_ERROR_CODE_ENDPOINT_UNAVAILABLE:    "EndpointUnavailable",
	corev1.ComponentPowerErrorCode_COMPONENT_POWER_ERROR_CODE_CREDENTIALS_UNAVAILABLE: "CredentialsUnavailable",
	corev1.ComponentPowerErrorCode_COMPONENT_POWER_ERROR_CODE_READ_FAILED:             "ReadFailed",
	corev1.ComponentPowerErrorCode_COMPONENT_POWER_ERROR_CODE_NO_OBSERVATION:          "NoObservation",
	corev1.ComponentPowerErrorCode_COMPONENT_POWER_ERROR_CODE_DEADLINE_EXCEEDED:       "DeadlineExceeded",
}

var powerActionStatusNames = map[corev1.ComponentPowerActionStatus]APIPowerActionStatus{
	corev1.ComponentPowerActionStatus_COMPONENT_POWER_ACTION_STATUS_ACTIVE:  "Active",
	corev1.ComponentPowerActionStatus_COMPONENT_POWER_ACTION_STATUS_PENDING: "Pending",
}

var powerActionOperationNames = map[corev1.ComponentPowerActionOperation]APIPowerActionOperation{
	corev1.ComponentPowerActionOperation_COMPONENT_POWER_ACTION_OPERATION_POWER_ON:      "PowerOn",
	corev1.ComponentPowerActionOperation_COMPONENT_POWER_ACTION_OPERATION_POWER_OFF:     "PowerOff",
	corev1.ComponentPowerActionOperation_COMPONENT_POWER_ACTION_OPERATION_RESET:         "Reset",
	corev1.ComponentPowerActionOperation_COMPONENT_POWER_ACTION_OPERATION_CHASSIS_RESET: "ChassisReset",
}

var powerTaskRelevanceNames = map[flowv1.PowerTaskRelevance]APIPowerTaskRelevance{
	flowv1.PowerTaskRelevance_POWER_TASK_RELEVANCE_SELECTED:      "Selected",
	flowv1.PowerTaskRelevance_POWER_TASK_RELEVANCE_RACK_AFFECTED: "RackAffected",
}

var powerTaskOperationNames = map[flowv1.PowerControlOperation]APIPowerTaskOperation{
	flowv1.PowerControlOperation_POWER_CONTROL_OPERATION_POWER_ON:        "PowerOn",
	flowv1.PowerControlOperation_POWER_CONTROL_OPERATION_FORCE_POWER_ON:  "ForcePowerOn",
	flowv1.PowerControlOperation_POWER_CONTROL_OPERATION_POWER_OFF:       "PowerOff",
	flowv1.PowerControlOperation_POWER_CONTROL_OPERATION_FORCE_POWER_OFF: "ForcePowerOff",
	flowv1.PowerControlOperation_POWER_CONTROL_OPERATION_RESTART:         "Restart",
	flowv1.PowerControlOperation_POWER_CONTROL_OPERATION_FORCE_RESTART:   "ForceRestart",
	flowv1.PowerControlOperation_POWER_CONTROL_OPERATION_WARM_RESET:      "WarmReset",
	flowv1.PowerControlOperation_POWER_CONTROL_OPERATION_COLD_RESET:      "ColdReset",
}

var taskTypeNames = map[flowv1.TaskKind]APITaskType{
	flowv1.TaskKind_TASK_KIND_POWER_CONTROL:      "PowerControl",
	flowv1.TaskKind_TASK_KIND_FIRMWARE_CONTROL:   "FirmwareControl",
	flowv1.TaskKind_TASK_KIND_BRING_UP:           "BringUp",
	flowv1.TaskKind_TASK_KIND_DECOMMISSION:       "Decommission",
	flowv1.TaskKind_TASK_KIND_INJECT_EXPECTATION: "InjectExpectation",
}

// APITrayPowerState uses inventory IDs; an unavailable identity stays nil.
type APITrayPowerState struct {
	// ID is the tray's external inventory component ID, or nil until the tray has a managed Core identity
	// or when the service doesn't supply it.
	ID *string `json:"id"`
	// Type is the tray's component type, or `Unknown` for an unrecognized service value.
	Type string `json:"type"`
	// RackID is the external inventory ID of the tray's assigned rack, including a retained soft-deleted rack,
	// or nil when the tray has no rack, the rack's identity can't be read, or the service doesn't supply it.
	RackID *string `json:"rackId"`
	// Actual is the live hardware observation, or why it failed.
	Actual APIPowerObservation `json:"actual"`
	// Desired is the stored compute power policy, read independently of the hardware.
	Desired APIPowerPolicy `json:"desired"`
	// Operations lists Core's tracked maintenance and Flow's outstanding power tasks.
	Operations APIPowerOperations `json:"operations"`
}

// APIPowerObservation distinguishes explicit `Unknown` from a failed observation.
type APIPowerObservation struct {
	// State is the observed power state, or nil when the read failed or the state can't be represented.
	State *APIPowerState `json:"state"`
	// Source is the selected hardware reader, or nil before backend selection or when unavailable.
	Source *APIPowerSource `json:"source"`
	// Observed is when NICo received the observation (not a device timestamp), or nil without one.
	Observed *time.Time `json:"observed"`
	// Error is the failure category, or nil when the observation succeeded.
	Error *APIPowerReadError `json:"error"`
}

// APIPowerPolicy reports stored host policy; switches and shelves have none.
type APIPowerPolicy struct {
	// Status is `Available` (including disabled power management), `Unavailable`, or
	// `NotApplicable` for managed switches and shelves. `Unavailable` also accompanies Error.
	Status APIPowerPolicyStatus `json:"status"`
	// State is the stored policy when Status is `Available`, otherwise nil.
	// `PowerManagerDisabled` is a policy, not missing information.
	State *APIPowerPolicyState `json:"state"`
	// Error is the failure category, or nil when the policy read succeeded.
	Error *APIPowerReadError `json:"error"`
}

// APIPowerOperations keeps Core maintenance separate from Flow task scheduling.
type APIPowerOperations struct {
	// Actions lists at most two tracked power maintenance actions from Core, `Active` before `Pending`;
	// empty when ActionsError is set.
	Actions []APIPowerAction `json:"actions"`
	// ActionsError explains why Actions couldn't be read, or is nil on success. It never describes an action.
	ActionsError *APIPowerReadError `json:"actionsError"`
	// Tasks lists Flow's outstanding power tasks that select or affect the tray, ordered by creation time
	// then ID; empty when TasksError is set.
	Tasks []APIPowerTask `json:"tasks"`
	// TasksError explains why Tasks couldn't be read, or is nil on success. It never describes a task.
	TasksError *APIPowerReadError `json:"tasksError"`
}

// APIPowerAction describes saved maintenance, without inventing a dispatch time or ID.
type APIPowerAction struct {
	// Status is `Active` for admitted maintenance (not hardware dispatch), `Pending` for a distinct saved
	// request without a promise of admission, or `Unknown` for an unrecognized service value.
	Status APIPowerActionStatus `json:"status"`
	// Operation is the recorded operation; `Reset` doesn't keep the original graceful, forced or AC-cycle choice.
	Operation APIPowerActionOperation `json:"operation"`
	// Requested is the saved submission time (not admission or start time), or nil for legacy admitted
	// requests without metadata.
	Requested *time.Time `json:"requested"`
	// Initiator is the saved initiator, which can be an empty string, or nil when unavailable.
	Initiator *string `json:"initiator"`
	// Graceful is set for `PowerOff` only: true is graceful and false is forced.
	Graceful *bool `json:"graceful"`
	// ChassisID is the recorded Redfish chassis for `ChassisReset`, otherwise nil.
	ChassisID *string `json:"chassisId"`
}

// APIPowerTask preserves task intent even when external identities cannot be read.
// RackID and Targets are nil together on TargetsError; other metadata stays usable.
type APIPowerTask struct {
	// ID is the Flow-generated task UUID, usable with the existing task APIs.
	ID string `json:"id"`
	// RackID is the external inventory ID of the task's recorded rack, or nil when TargetsError is set.
	RackID *string `json:"rackId"`
	// Operation is the requested Flow power operation, separate from the operation Core stores.
	Operation APIPowerTaskOperation `json:"operation"`
	// OverrideReadinessCheck is true when the task bypasses the component readiness gate.
	OverrideReadinessCheck bool `json:"overrideReadinessCheck"`
	// Targets is the complete recorded selection grouped by component type, or nil when TargetsError is set.
	Targets []APIPowerTaskTargetGroup `json:"targets"`
	// TargetsError explains why RackID and Targets couldn't be read, or is nil on success.
	TargetsError *APIPowerReadError `json:"targetsError"`
	// Status is the public task status; Flow `Waiting` and admitted `Pending` tasks both report `Pending`.
	Status string `json:"status"`
	// Created is the task creation time, or nil without a usable timestamp.
	Created *time.Time `json:"created"`
	// Updated is the last recorded task update time, or nil when unavailable.
	Updated *time.Time `json:"updated"`
	// Started is the recorded execution start, or nil until recorded.
	Started *time.Time `json:"started"`
	// Relevance is `Selected` when the tray is a recorded target, or `RackAffected` when the task
	// selects a shelf in the tray's current rack.
	Relevance APIPowerTaskRelevance `json:"relevance"`
	// WaitingContext describes other work in the rack for a Flow `Waiting` task, and is nil for every other
	// task. When an older service doesn't supply it, both lists are empty and Error is `Unsupported`.
	WaitingContext *APIPowerTaskWaitingContext `json:"waitingContext"`
}

// APIPowerTaskTargetGroup preserves the recorded selection's type and order,
// using external IDs read from retained inventory, including soft-deleted rows.
type APIPowerTaskTargetGroup struct {
	// Type is the recorded component type.
	Type string `json:"type"`
	// ComponentIDs lists external inventory component IDs in recorded selection order.
	ComponentIDs []string `json:"componentIds"`
}

// APIPowerTaskWaitingContext supplies scheduling context, without promising a blocker or queue position.
type APIPowerTaskWaitingContext struct {
	// ActiveConflicts lists `Pending` or `Running` tasks that conflict under the current scheduler rule,
	// ordered by creation time then ID.
	ActiveConflicts []APITaskSummary `json:"activeConflicts"`
	// EarlierWaiting lists every strictly earlier `Waiting` task in the rack, ordered by creation time then ID.
	EarlierWaiting []APITaskSummary `json:"earlierWaiting"`
	// Error explains why the context couldn't be read, or is nil on success.
	Error *APIPowerReadError `json:"error"`
}

// APITaskSummary identifies an active conflict or earlier waiting task of any type.
type APITaskSummary struct {
	// ID is the Flow-generated task UUID.
	ID string `json:"id"`
	// Type is the task's category, which can be non-power work such as `BringUp`.
	Type APITaskType `json:"type"`
	// Status is the public task status; Flow `Waiting` maps to `Pending`.
	Status string `json:"status"`
}

// FromProto replaces every section, retaining failures independently of successful values.
func (d *APITrayPowerState) FromProto(p *flowv1.TrayPowerDetails) {
	*d = APITrayPowerState{
		Type: enumOr(ProtoToAPIComponentTypeName, p.GetComponentType(), "Unknown"),
	}
	if p != nil {
		d.ID = p.ComponentId
		d.RackID = p.RackExternalId
	}
	d.Actual.FromProto(p.GetActual())
	d.Desired.FromProto(p.GetDesired())
	d.Operations.FromProto(p.GetOperations())
}

// FromProto preserves observation presence rather than treating a getter's zero as `Unknown`.
func (d *APIPowerObservation) FromProto(p *flowv1.PowerActualDetails) {
	*d = APIPowerObservation{Error: cutil.GetPtr[APIPowerReadError]("Unsupported")}
	if p == nil {
		return
	}
	e, isError := p.Result.(*flowv1.PowerActualDetails_Error)
	if isError {
		d.Error.FromProto(e.Error)
		return
	}
	value := p.GetValue()
	if value == nil {
		return
	}
	var source APIPowerSource
	source.FromProto(value.Source)
	if source != "" {
		d.Source = &source
	}
	switch observation := value.Outcome.(type) {
	case *corev1.ComponentPowerObservation_State:
		var state APIPowerState
		state.FromProto(observation.State)
		if state == "" {
			return
		}
		d.State, d.Observed, d.Error = &state, powerStateTimestamp(value.ObservedAt), nil
	case *corev1.ComponentPowerObservation_Error:
		d.Error.FromCoreProto(observation.Error.GetCode())
	default:
		d.Error = cutil.GetPtr[APIPowerReadError]("InvalidResponse")
	}
}

// FromProto keeps a disabled policy distinct from missing or inapplicable policy.
func (d *APIPowerPolicy) FromProto(p *flowv1.PowerDesiredDetails) {
	*d = APIPowerPolicy{Status: "Unavailable", Error: cutil.GetPtr[APIPowerReadError]("Unsupported")}
	if p == nil {
		return
	}
	e, isError := p.Result.(*flowv1.PowerDesiredDetails_Error)
	if isError {
		d.Error.FromProto(e.Error)
		return
	}
	value := p.GetValue()
	if value == nil {
		return
	}
	var status APIPowerPolicyStatus
	status.FromProto(value.Status)
	switch status {
	case "Unavailable":
		d.Error = nil
	case "NotApplicable":
		d.Status, d.Error = status, nil
	case "Available":
		if value.State == nil {
			d.Error = cutil.GetPtr[APIPowerReadError]("InvalidResponse")
			return
		}
		var state APIPowerPolicyState
		state.FromProto(*value.State)
		if state == "" {
			return
		}
		d.Status = status
		d.State = &state
		d.Error = nil
	}
}

// FromProto reads Core actions and Flow tasks separately, so either list can fail alone.
func (d *APIPowerOperations) FromProto(p *flowv1.PowerOperations) {
	d.actionsFromProto(p.GetActions())
	d.tasksFromProto(p.GetTasks())
}

// actionsFromProto returns an explicit empty list on success and retains a failed list's error.
func (d *APIPowerOperations) actionsFromProto(p *flowv1.PowerActionDetails) {
	d.Actions, d.ActionsError = []APIPowerAction{}, cutil.GetPtr[APIPowerReadError]("Unsupported")
	if p == nil {
		return
	}
	e, isError := p.Result.(*flowv1.PowerActionDetails_Error)
	if isError {
		d.ActionsError.FromProto(e.Error)
		return
	}
	value := p.GetValue()
	if value == nil {
		return
	}
	if value.Error != nil {
		d.ActionsError.FromCoreProto(value.Error.Code)
		return
	}
	d.ActionsError = nil
	for _, action := range value.Actions {
		entry := APIPowerAction{
			Requested: powerStateTimestamp(action.GetRequestedAt()),
			Initiator: action.Initiator,
			Graceful:  action.Graceful,
			ChassisID: action.ChassisId,
		}
		entry.Status.FromProto(action.GetStatus())
		entry.Operation.FromProto(action.GetOperation())
		d.Actions = append(d.Actions, entry)
	}
}

// tasksFromProto preserves task order and treats a missing list as unsupported, not empty.
func (d *APIPowerOperations) tasksFromProto(p *flowv1.PowerTaskDetailsList) {
	d.Tasks, d.TasksError = []APIPowerTask{}, cutil.GetPtr[APIPowerReadError]("Unsupported")
	if p == nil {
		return
	}
	if p.Error != nil {
		d.TasksError.FromProto(*p.Error)
		return
	}
	d.TasksError = nil
	for _, task := range p.Entries {
		entry := APIPowerTask{}
		entry.FromProto(task)
		d.Tasks = append(d.Tasks, entry)
	}
}

// FromProto publishes identities only when the complete recorded selection matches.
// TargetsError explains unavailable identities while other task metadata stays usable.
func (d *APIPowerTask) FromProto(p *flowv1.PowerTaskDetails) {
	*d = APIPowerTask{
		ID:           p.GetId().GetId(),
		Status:       enumOr(ProtoToAPITaskStatusName, p.GetStatus(), "Unknown"),
		Created:      powerStateTimestamp(p.GetCreatedAt()),
		Updated:      powerStateTimestamp(p.GetUpdatedAt()),
		Started:      powerStateTimestamp(p.GetStartedAt()),
		TargetsError: cutil.GetPtr[APIPowerReadError]("Unsupported"),
	}
	operation := p.GetOperation().GetPowerControl()
	d.Operation.FromProto(operation.GetOperation())
	d.Relevance.FromProto(p.GetRelevance())
	d.OverrideReadinessCheck = operation.GetOverrideReadinessCheck()
	// REST reports Flow `Waiting` tasks as `Pending`, so a null context must mean the task isn't waiting.
	// A Waiting task from an older service that omits the field reports `Unsupported` instead.
	if p.GetWaitingContext() != nil || p.GetStatus() == flowv1.TaskStatus_TASK_STATUS_WAITING {
		d.WaitingContext = &APIPowerTaskWaitingContext{}
		d.WaitingContext.FromProto(p.GetWaitingContext())
	}

	external := p.GetExternalTargets()
	if external == nil {
		return
	}
	if external.Error != nil {
		d.TargetsError.FromProto(*external.Error)
		return
	}
	d.TargetsError = cutil.GetPtr[APIPowerReadError]("InvalidResponse")
	if external.RackId == "" {
		return
	}

	// Publish identities only after the complete recorded selection matches.
	// Otherwise a malformed success could make a task appear to target fewer trays.
	targets := []APIPowerTaskTargetGroup{}
	nextTarget := 0
	for _, group := range p.GetTargets().GetGroups() {
		if len(group.GetComponentIds()) == 0 {
			continue
		}
		componentType, known := ProtoToAPIRackComponentTypeName[group.GetType()]
		if !known || group.GetType() == flowv1.ComponentType_COMPONENT_TYPE_UNKNOWN {
			return
		}
		entry := APIPowerTaskTargetGroup{Type: componentType, ComponentIDs: []string{}}
		for range group.GetComponentIds() {
			if nextTarget >= len(external.Targets) {
				return
			}
			target := external.Targets[nextTarget]
			if target.GetType() != group.GetType() || target.GetId() == "" {
				return
			}
			entry.ComponentIDs = append(entry.ComponentIDs, target.GetId())
			nextTarget++
		}
		targets = append(targets, entry)
	}
	if nextTarget != len(external.Targets) {
		return
	}
	d.RackID, d.Targets, d.TargetsError = &external.RackId, targets, nil
}

// FromProto clears stale context. Nil input means the service didn't supply
// context, so it reports `Unsupported` rather than an evaluated empty result.
func (d *APIPowerTaskWaitingContext) FromProto(p *flowv1.PowerTaskWaitingContext) {
	*d = APIPowerTaskWaitingContext{ActiveConflicts: []APITaskSummary{}, EarlierWaiting: []APITaskSummary{}}
	if p == nil {
		d.Error = cutil.GetPtr[APIPowerReadError]("Unsupported")
		return
	}
	if p.Error != nil {
		d.Error = new(APIPowerReadError)
		d.Error.FromProto(*p.Error)
		return
	}
	for _, task := range p.ActiveConflicts {
		entry := APITaskSummary{}
		entry.FromProto(task)
		d.ActiveConflicts = append(d.ActiveConflicts, entry)
	}
	for _, task := range p.EarlierWaiting {
		entry := APITaskSummary{}
		entry.FromProto(task)
		d.EarlierWaiting = append(d.EarlierWaiting, entry)
	}
}

// FromProto restricts scheduling context to task identity, type and public status.
func (d *APITaskSummary) FromProto(p *flowv1.TaskBrief) {
	*d = APITaskSummary{ID: p.GetId().GetId(), Status: enumOr(ProtoToAPITaskStatusName, p.GetStatus(), "Unknown")}
	d.Type.FromProto(p.GetKind())
}

// FromProto clears an unrecognized state; an explicit protobuf Unknown remains known.
func (s *APIPowerState) FromProto(state corev1.ComponentPowerState) {
	*s = powerStateNames[state]
}

// FromProto maps Flow failures to public categories, including malformed error values.
func (e *APIPowerReadError) FromProto(code flowv1.PowerDetailsReadError) {
	*e = enumOr(powerReadErrorNames, code, "InvalidResponse")
}

// FromCoreProto maps per-component failures without exposing dependency text.
func (e *APIPowerReadError) FromCoreProto(code corev1.ComponentPowerErrorCode) {
	*e = enumOr(corePowerErrorNames, code, "InvalidResponse")
}

// FromProto clears unrecognized policy values so the enclosing section stays unavailable.
func (s *APIPowerPolicyState) FromProto(state corev1.PowerState) {
	*s = powerPolicyStateNames[state]
}

// FromProto distinguishes known policy availability from an unsupported status.
func (s *APIPowerPolicyStatus) FromProto(status corev1.ComponentPowerPolicyStatus) {
	*s = ""
	switch status {
	case corev1.ComponentPowerPolicyStatus_COMPONENT_POWER_POLICY_STATUS_AVAILABLE:
		*s = "Available"
	case corev1.ComponentPowerPolicyStatus_COMPONENT_POWER_POLICY_STATUS_UNAVAILABLE:
		*s = "Unavailable"
	case corev1.ComponentPowerPolicyStatus_COMPONENT_POWER_POLICY_STATUS_NOT_APPLICABLE:
		*s = "NotApplicable"
	}
}

// FromProto retains known maintenance status and marks newer values Unknown.
func (s *APIPowerActionStatus) FromProto(status corev1.ComponentPowerActionStatus) {
	*s = enumOr(powerActionStatusNames, status, "Unknown")
}

// FromProto retains known maintenance operations and marks newer values Unknown.
func (o *APIPowerActionOperation) FromProto(operation corev1.ComponentPowerActionOperation) {
	*o = enumOr(powerActionOperationNames, operation, "Unknown")
}

// FromProto retains known task operations and marks newer values Unknown.
func (o *APIPowerTaskOperation) FromProto(operation flowv1.PowerControlOperation) {
	*o = enumOr(powerTaskOperationNames, operation, "Unknown")
}

// FromProto retains the selection relationship and marks newer values Unknown.
func (r *APIPowerTaskRelevance) FromProto(relevance flowv1.PowerTaskRelevance) {
	*r = enumOr(powerTaskRelevanceNames, relevance, "Unknown")
}

// FromProto retains known task types and marks newer values Unknown.
func (t *APITaskType) FromProto(kind flowv1.TaskKind) {
	*t = enumOr(taskTypeNames, kind, "Unknown")
}

// FromProto clears absent or unrecognized sources; the enclosing section owns
// whether that absence is represented by a nil field.
func (s *APIPowerSource) FromProto(source *corev1.ComponentPowerSource) {
	*s = ""
	if source == nil {
		return
	}
	*s = powerSourceNames[*source]
}

// powerStateTimestamp preserves missing or invalid values as nil.
func powerStateTimestamp(timestamp *timestamppb.Timestamp) *time.Time {
	if timestamp == nil || timestamp.CheckValid() != nil {
		return nil
	}
	value := timestamp.AsTime()
	return &value
}
