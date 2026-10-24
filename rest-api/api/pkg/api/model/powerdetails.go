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

// APIPowerSource identifies the hardware reader or stored policy behind a value.
type APIPowerSource string

// APIPowerDetailsError is a bounded failure category, without dependency text.
type APIPowerDetailsError string

var powerStateNames = map[corev1.ComponentPowerState]APIPowerState{
	corev1.ComponentPowerState_COMPONENT_POWER_STATE_UNKNOWN:      "Unknown",
	corev1.ComponentPowerState_COMPONENT_POWER_STATE_ON:           "On",
	corev1.ComponentPowerState_COMPONENT_POWER_STATE_OFF:          "Off",
	corev1.ComponentPowerState_COMPONENT_POWER_STATE_POWERING_ON:  "PoweringOn",
	corev1.ComponentPowerState_COMPONENT_POWER_STATE_POWERING_OFF: "PoweringOff",
	corev1.ComponentPowerState_COMPONENT_POWER_STATE_PAUSED:       "Paused",
	corev1.ComponentPowerState_COMPONENT_POWER_STATE_RESET:        "Reset",
}

var powerPolicyStateNames = map[corev1.PowerState]string{
	corev1.PowerState_On:                   "On",
	corev1.PowerState_Off:                  "Off",
	corev1.PowerState_PowerManagerDisabled: "PowerManagerDisabled",
}

var powerSourceNames = map[corev1.ComponentPowerSource]APIPowerSource{
	corev1.ComponentPowerSource_COMPONENT_POWER_SOURCE_REDFISH:           "Redfish",
	corev1.ComponentPowerSource_COMPONENT_POWER_SOURCE_RMS:               "Rms",
	corev1.ComponentPowerSource_COMPONENT_POWER_SOURCE_CORE_POWER_POLICY: "CorePowerPolicy",
}

var powerReadErrorNames = map[flowv1.PowerDetailsReadError]APIPowerDetailsError{
	flowv1.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_NOT_LINKED:        "NotLinked",
	flowv1.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_NOT_CONFIGURED:    "NotConfigured",
	flowv1.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_UNSUPPORTED:       "Unsupported",
	flowv1.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_DEADLINE_EXCEEDED: "DeadlineExceeded",
	flowv1.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_READ_FAILED:       "ReadFailed",
	flowv1.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_INVALID_RESPONSE:  "InvalidResponse",
	flowv1.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_LIMIT_EXCEEDED:    "LimitExceeded",
}

var corePowerErrorNames = map[corev1.ComponentPowerErrorCode]APIPowerDetailsError{
	corev1.ComponentPowerErrorCode_COMPONENT_POWER_ERROR_CODE_NOT_FOUND:               "NotFound",
	corev1.ComponentPowerErrorCode_COMPONENT_POWER_ERROR_CODE_DISABLED:                "Disabled",
	corev1.ComponentPowerErrorCode_COMPONENT_POWER_ERROR_CODE_UNSUPPORTED:             "Unsupported",
	corev1.ComponentPowerErrorCode_COMPONENT_POWER_ERROR_CODE_ENDPOINT_UNAVAILABLE:    "EndpointUnavailable",
	corev1.ComponentPowerErrorCode_COMPONENT_POWER_ERROR_CODE_CREDENTIALS_UNAVAILABLE: "CredentialsUnavailable",
	corev1.ComponentPowerErrorCode_COMPONENT_POWER_ERROR_CODE_READ_FAILED:             "ReadFailed",
	corev1.ComponentPowerErrorCode_COMPONENT_POWER_ERROR_CODE_NO_OBSERVATION:          "NoObservation",
	corev1.ComponentPowerErrorCode_COMPONENT_POWER_ERROR_CODE_DEADLINE_EXCEEDED:       "DeadlineExceeded",
}

var powerActionStatusNames = map[corev1.ComponentPowerActionStatus]string{
	corev1.ComponentPowerActionStatus_COMPONENT_POWER_ACTION_STATUS_ACTIVE:  "Active",
	corev1.ComponentPowerActionStatus_COMPONENT_POWER_ACTION_STATUS_PENDING: "Pending",
}

var powerActionOperationNames = map[corev1.ComponentPowerActionOperation]string{
	corev1.ComponentPowerActionOperation_COMPONENT_POWER_ACTION_OPERATION_POWER_ON:      "PowerOn",
	corev1.ComponentPowerActionOperation_COMPONENT_POWER_ACTION_OPERATION_POWER_OFF:     "PowerOff",
	corev1.ComponentPowerActionOperation_COMPONENT_POWER_ACTION_OPERATION_RESET:         "Reset",
	corev1.ComponentPowerActionOperation_COMPONENT_POWER_ACTION_OPERATION_CHASSIS_RESET: "ChassisReset",
}

var powerTaskRelevanceNames = map[flowv1.PowerTaskRelevance]string{
	flowv1.PowerTaskRelevance_POWER_TASK_RELEVANCE_SELECTED:      "Selected",
	flowv1.PowerTaskRelevance_POWER_TASK_RELEVANCE_RACK_AFFECTED: "RackAffected",
}

var powerTaskOperationNames = map[flowv1.PowerControlOperation]string{
	flowv1.PowerControlOperation_POWER_CONTROL_OPERATION_POWER_ON:        "PowerOn",
	flowv1.PowerControlOperation_POWER_CONTROL_OPERATION_FORCE_POWER_ON:  "ForcePowerOn",
	flowv1.PowerControlOperation_POWER_CONTROL_OPERATION_POWER_OFF:       "PowerOff",
	flowv1.PowerControlOperation_POWER_CONTROL_OPERATION_FORCE_POWER_OFF: "ForcePowerOff",
	flowv1.PowerControlOperation_POWER_CONTROL_OPERATION_RESTART:         "Restart",
	flowv1.PowerControlOperation_POWER_CONTROL_OPERATION_FORCE_RESTART:   "ForceRestart",
	flowv1.PowerControlOperation_POWER_CONTROL_OPERATION_WARM_RESET:      "WarmReset",
	flowv1.PowerControlOperation_POWER_CONTROL_OPERATION_COLD_RESET:      "ColdReset",
}

var powerTaskKindNames = map[flowv1.TaskKind]string{
	flowv1.TaskKind_TASK_KIND_POWER_CONTROL:      "PowerControl",
	flowv1.TaskKind_TASK_KIND_FIRMWARE_CONTROL:   "FirmwareControl",
	flowv1.TaskKind_TASK_KIND_BRING_UP:           "BringUp",
	flowv1.TaskKind_TASK_KIND_DECOMMISSION:       "Decommission",
	flowv1.TaskKind_TASK_KIND_INJECT_EXPECTATION: "InjectExpectation",
}

// APITrayPowerDetails keeps inventory identity usable before Core links a tray.
// The two Flow UUIDs are distinct from the public inventory's external IDs.
type APITrayPowerDetails struct {
	FlowComponentID string                 `json:"flowComponentId"`
	CoreComponentID *string                `json:"coreComponentId"`
	Type            string                 `json:"type"`
	FlowRackID      *string                `json:"flowRackId"`
	Actual          APIPowerActualDetails  `json:"actual"`
	Desired         APIPowerDesiredDetails `json:"desired"`
	Operations      APIPowerOperations     `json:"operations"`
}

// APIPowerActualDetails distinguishes explicit `Unknown` from a failed observation.
type APIPowerActualDetails struct {
	State      *APIPowerState        `json:"state"`
	Source     *APIPowerSource       `json:"source"`
	ObservedAt *time.Time            `json:"observedAt"`
	Error      *APIPowerDetailsError `json:"error"`
}

// APIPowerDesiredDetails reports stored host policy; switches and shelves have none.
type APIPowerDesiredDetails struct {
	Status    string                `json:"status"`
	State     *string               `json:"state"`
	Source    *APIPowerSource       `json:"source"`
	UpdatedAt *time.Time            `json:"updatedAt"`
	Error     *APIPowerDetailsError `json:"error"`
}

// APIPowerOperations keeps Core maintenance separate from Flow task scheduling.
type APIPowerOperations struct {
	Actions APIPowerActionDetails   `json:"actions"`
	Tasks   APIPowerTaskDetailsList `json:"tasks"`
}

// APIPowerActionDetails preserves the service's `Active` then `Pending` order;
// an error invalidates the list.
type APIPowerActionDetails struct {
	Entries []APIPowerAction      `json:"entries"`
	Error   *APIPowerDetailsError `json:"error"`
}

// APIPowerAction describes saved maintenance, without inventing a dispatch time or ID.
type APIPowerAction struct {
	Status      string     `json:"status"`
	Operation   string     `json:"operation"`
	RequestedAt *time.Time `json:"requestedAt"`
	Initiator   *string    `json:"initiator"`
	Graceful    *bool      `json:"graceful"`
	ChassisID   *string    `json:"chassisId"`
}

// APIPowerTaskDetailsList contains outstanding power tasks in Flow's stable order.
type APIPowerTaskDetailsList struct {
	Entries []APIPowerTaskDetails `json:"entries"`
	Error   *APIPowerDetailsError `json:"error"`
}

// APIPowerTaskDetails preserves task intent and the recorded selection, not live inventory.
type APIPowerTaskDetails struct {
	ID                     string                      `json:"id"`
	FlowRackID             string                      `json:"flowRackId"`
	Operation              string                      `json:"operation"`
	OverrideReadinessCheck bool                        `json:"overrideReadinessCheck"`
	Targets                []APIPowerTaskTargets       `json:"targets"`
	Status                 string                      `json:"status"`
	CreatedAt              *time.Time                  `json:"createdAt"`
	UpdatedAt              *time.Time                  `json:"updatedAt"`
	StartedAt              *time.Time                  `json:"startedAt"`
	Relevance              string                      `json:"relevance"`
	WaitingContext         *APIPowerTaskWaitingContext `json:"waitingContext"`
}

// APIPowerTaskTargets groups recorded Flow component UUIDs by tray type.
type APIPowerTaskTargets struct {
	Type             string   `json:"type"`
	FlowComponentIDs []string `json:"flowComponentIds"`
}

// APIPowerTaskWaitingContext supplies scheduling context, without promising a blocker or queue position.
type APIPowerTaskWaitingContext struct {
	ActiveConflicts []APIPowerTaskBrief   `json:"activeConflicts"`
	EarlierWaiting  []APIPowerTaskBrief   `json:"earlierWaiting"`
	Error           *APIPowerDetailsError `json:"error"`
}

// APIPowerTaskBrief exposes only task identity, kind and public status for nearby work.
type APIPowerTaskBrief struct {
	ID     string `json:"id"`
	Kind   string `json:"kind"`
	Status string `json:"status"`
}

// FromProto replaces every section, retaining failures independently of successful values.
func (d *APITrayPowerDetails) FromProto(p *flowv1.TrayPowerDetails) {
	*d = APITrayPowerDetails{
		FlowComponentID: p.GetFlowComponentId().GetId(),
		Type:            enumOr(ProtoToAPIComponentTypeName, p.GetComponentType(), "Unknown"),
	}
	if p != nil {
		d.CoreComponentID = p.CoreComponentId
	}
	if p.GetRackId() != nil {
		id := p.GetRackId().GetId()
		d.FlowRackID = &id
	}
	d.Actual.FromProto(p.GetActual())
	d.Desired.FromProto(p.GetDesired())
	d.Operations.Actions.FromProto(p.GetOperations().GetActions())
	d.Operations.Tasks.FromProto(p.GetOperations().GetTasks())
}

// FromProto preserves observation presence rather than treating a getter's zero as `Unknown`.
func (d *APIPowerActualDetails) FromProto(p *flowv1.PowerActualDetails) {
	*d = APIPowerActualDetails{Error: cutil.GetPtr[APIPowerDetailsError]("Unsupported")}
	if p == nil {
		return
	}
	e, isError := p.Result.(*flowv1.PowerActualDetails_Error)
	if isError {
		d.Error = cutil.GetPtr[APIPowerDetailsError](enumOr(powerReadErrorNames, e.Error, "InvalidResponse"))
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
		state, ok := powerStateNames[observation.State]
		if !ok {
			return
		}
		d.State, d.ObservedAt, d.Error = &state, powerDetailsTimestamp(value.ObservedAt), nil
	case *corev1.ComponentPowerObservation_Error:
		d.Error = cutil.GetPtr[APIPowerDetailsError](enumOr(corePowerErrorNames, observation.Error.GetCode(), "InvalidResponse"))
	}
}

// FromProto keeps a disabled policy distinct from missing or inapplicable policy.
func (d *APIPowerDesiredDetails) FromProto(p *flowv1.PowerDesiredDetails) {
	*d = APIPowerDesiredDetails{Status: "Unavailable", Error: cutil.GetPtr[APIPowerDetailsError]("Unsupported")}
	if p == nil {
		return
	}
	e, isError := p.Result.(*flowv1.PowerDesiredDetails_Error)
	if isError {
		d.Error = cutil.GetPtr[APIPowerDetailsError](enumOr(powerReadErrorNames, e.Error, "InvalidResponse"))
		return
	}
	value := p.GetValue()
	if value == nil {
		return
	}
	switch value.Status {
	case corev1.ComponentPowerPolicyStatus_COMPONENT_POWER_POLICY_STATUS_UNAVAILABLE:
		d.Error = nil
	case corev1.ComponentPowerPolicyStatus_COMPONENT_POWER_POLICY_STATUS_NOT_APPLICABLE:
		d.Status, d.Error = "NotApplicable", nil
	case corev1.ComponentPowerPolicyStatus_COMPONENT_POWER_POLICY_STATUS_AVAILABLE:
		if value.State == nil {
			d.Error = cutil.GetPtr[APIPowerDetailsError]("InvalidResponse")
			return
		}
		state, ok := powerPolicyStateNames[*value.State]
		if !ok {
			return
		}
		d.Status = "Available"
		d.State = &state
		var source APIPowerSource
		source.FromProto(value.Source)
		if source != "" {
			d.Source = &source
		}
		d.UpdatedAt = powerDetailsTimestamp(value.UpdatedAt)
		d.Error = nil
	}
}

// FromProto returns an explicit empty list on success and retains a failed section's error.
func (d *APIPowerActionDetails) FromProto(p *flowv1.PowerActionDetails) {
	*d = APIPowerActionDetails{Entries: []APIPowerAction{}, Error: cutil.GetPtr[APIPowerDetailsError]("Unsupported")}
	if p == nil {
		return
	}
	e, isError := p.Result.(*flowv1.PowerActionDetails_Error)
	if isError {
		d.Error = cutil.GetPtr[APIPowerDetailsError](enumOr(powerReadErrorNames, e.Error, "InvalidResponse"))
		return
	}
	value := p.GetValue()
	if value == nil {
		return
	}
	if value.Error != nil {
		d.Error = cutil.GetPtr[APIPowerDetailsError](enumOr(corePowerErrorNames, value.Error.Code, "InvalidResponse"))
		return
	}
	d.Error = nil
	for _, action := range value.Actions {
		entry := APIPowerAction{
			RequestedAt: powerDetailsTimestamp(action.GetRequestedAt()),
			Initiator:   action.Initiator,
			Graceful:    action.Graceful,
			ChassisID:   action.ChassisId,
		}
		entry.Status = enumOr(powerActionStatusNames, action.GetStatus(), "Unknown")
		entry.Operation = enumOr(powerActionOperationNames, action.GetOperation(), "Unknown")
		d.Entries = append(d.Entries, entry)
	}
}

// FromProto preserves task order and treats a missing section as unsupported, not empty.
func (d *APIPowerTaskDetailsList) FromProto(p *flowv1.PowerTaskDetailsList) {
	*d = APIPowerTaskDetailsList{Entries: []APIPowerTaskDetails{}, Error: cutil.GetPtr[APIPowerDetailsError]("Unsupported")}
	if p == nil {
		return
	}
	if p.Error != nil {
		d.Error = cutil.GetPtr[APIPowerDetailsError](enumOr(powerReadErrorNames, *p.Error, "InvalidResponse"))
		return
	}
	d.Error = nil
	for _, task := range p.Entries {
		entry := APIPowerTaskDetails{}
		entry.FromProto(task)
		d.Entries = append(d.Entries, entry)
	}
}

// FromProto maps Flow `Waiting` to the existing public `Pending` status; context remains separate.
func (d *APIPowerTaskDetails) FromProto(p *flowv1.PowerTaskDetails) {
	*d = APIPowerTaskDetails{
		ID:         p.GetId().GetId(),
		FlowRackID: p.GetRackId().GetId(),
		Status:     enumOr(ProtoToAPITaskStatusName, p.GetStatus(), "Unknown"),
		CreatedAt:  powerDetailsTimestamp(p.GetCreatedAt()),
		UpdatedAt:  powerDetailsTimestamp(p.GetUpdatedAt()),
		StartedAt:  powerDetailsTimestamp(p.GetStartedAt()),
		Relevance:  enumOr(powerTaskRelevanceNames, p.GetRelevance(), "Unknown"),
	}
	operation := p.GetOperation().GetPowerControl()
	d.Operation = enumOr(powerTaskOperationNames, operation.GetOperation(), "Unknown")
	d.OverrideReadinessCheck = operation.GetOverrideReadinessCheck()
	if p.GetTargets() != nil {
		d.Targets = []APIPowerTaskTargets{}
		for _, group := range p.GetTargets().GetGroups() {
			targets := APIPowerTaskTargets{Type: enumOr(ProtoToAPIComponentTypeName, group.GetType(), "Unknown"), FlowComponentIDs: []string{}}
			for _, id := range group.GetComponentIds() {
				targets.FlowComponentIDs = append(targets.FlowComponentIDs, id.GetId())
			}
			d.Targets = append(d.Targets, targets)
		}
	}
	if p.GetWaitingContext() != nil {
		d.WaitingContext = &APIPowerTaskWaitingContext{}
		d.WaitingContext.FromProto(p.GetWaitingContext())
	}
}

// FromProto clears stale context, including on nil input. The enclosing task
// keeps an absent older-service field distinct from an evaluated empty context.
func (d *APIPowerTaskWaitingContext) FromProto(p *flowv1.PowerTaskWaitingContext) {
	*d = APIPowerTaskWaitingContext{ActiveConflicts: []APIPowerTaskBrief{}, EarlierWaiting: []APIPowerTaskBrief{}}
	if p == nil {
		return
	}
	if p.Error != nil {
		d.Error = cutil.GetPtr[APIPowerDetailsError](enumOr(powerReadErrorNames, *p.Error, "InvalidResponse"))
		return
	}
	for _, task := range p.ActiveConflicts {
		entry := APIPowerTaskBrief{}
		entry.FromProto(task)
		d.ActiveConflicts = append(d.ActiveConflicts, entry)
	}
	for _, task := range p.EarlierWaiting {
		entry := APIPowerTaskBrief{}
		entry.FromProto(task)
		d.EarlierWaiting = append(d.EarlierWaiting, entry)
	}
}

// FromProto restricts nearby tasks of any kind to identity and scheduling metadata.
func (d *APIPowerTaskBrief) FromProto(p *flowv1.TaskBrief) {
	*d = APIPowerTaskBrief{ID: p.GetId().GetId(), Status: enumOr(ProtoToAPITaskStatusName, p.GetStatus(), "Unknown")}
	d.Kind = enumOr(powerTaskKindNames, p.GetKind(), "Unknown")
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

// powerDetailsTimestamp preserves missing or invalid values as nil.
func powerDetailsTimestamp(timestamp *timestamppb.Timestamp) *time.Time {
	if timestamp == nil || timestamp.CheckValid() != nil {
		return nil
	}
	value := timestamp.AsTime()
	return &value
}
