// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
)

// APIRackPowerState preserves Flow's member evidence and synthesized summaries.
type APIRackPowerState struct {
	// ID is the external inventory rack ID, or nil when the service omits it.
	ID *string `json:"id"`
	// Actual summarizes live observations with coverage supplied by Flow.
	Actual APIRackPowerSummary `json:"actual"`
	// Desired includes coverage for every policy; only enabled `On` and `Off` policies vote.
	Desired APIRackPowerSummary `json:"desired"`
	// Operations contains each full Flow power task once.
	Operations APIRackPowerOperations `json:"operations"`
	// Components includes every current inventory member, including unlinked and unsupported types.
	Components []APIRackComponentPowerState `json:"components"`
}

// APIRackPowerSummaryState includes observed states and Flow's synthesized `Mixed` state.
type APIRackPowerSummaryState string

var rackPowerStateNames = map[flowv1.RackPowerState]APIRackPowerSummaryState{
	flowv1.RackPowerState_RACK_POWER_STATE_UNKNOWN:      "Unknown",
	flowv1.RackPowerState_RACK_POWER_STATE_ON:           "On",
	flowv1.RackPowerState_RACK_POWER_STATE_OFF:          "Off",
	flowv1.RackPowerState_RACK_POWER_STATE_POWERING_ON:  "PoweringOn",
	flowv1.RackPowerState_RACK_POWER_STATE_POWERING_OFF: "PoweringOff",
	flowv1.RackPowerState_RACK_POWER_STATE_PAUSED:       "Paused",
	flowv1.RackPowerState_RACK_POWER_STATE_RESET:        "Reset",
	flowv1.RackPowerState_RACK_POWER_STATE_MIXED:        "Mixed",
}

// APIRackPowerSummary reports a synthesized state alongside its contributing evidence.
type APIRackPowerSummary struct {
	// State is `Unknown` for absent or unrecognized service states, without changing the supplied coverage.
	State APIRackPowerSummaryState `json:"state"`
	// Coverage preserves Flow's counts; REST leaf error mappings do not reclassify them.
	Coverage APIRackPowerCoverage `json:"coverage"`
	// Complete is Flow's decision that unknown and unavailable counts are zero.
	// Unsupported members and absent votes do not prevent true; an omitted summary is false.
	Complete bool `json:"complete"`
}

// APIRackPowerCoverage preserves Flow's mutually exclusive member counts.
type APIRackPowerCoverage struct {
	// Total counts every current inventory member.
	Total uint32 `json:"total"`
	// Known counts known observations or enabled `On` and `Off` policy votes.
	Known uint32 `json:"known"`
	// Unknown counts explicit hardware observations; it is zero for desired policy.
	Unknown uint32 `json:"unknown"`
	// Unavailable counts failed, unlinked or unrepresentable reads as classified by Flow.
	Unavailable uint32 `json:"unavailable"`
	// NotApplicable, NoPolicy and DisabledPolicy describe desired members that do not vote as `On` or `Off`.
	NotApplicable  uint32 `json:"notApplicable"`
	NoPolicy       uint32 `json:"noPolicy"`
	DisabledPolicy uint32 `json:"disabledPolicy"`
	// Unsupported counts unsupported inventory types or backend observations as classified by Flow.
	Unsupported uint32 `json:"unsupported"`
}

// APIRackPowerOperations holds each full Flow power task once.
type APIRackPowerOperations struct {
	// Tasks preserves all outstanding power tasks ordered by creation time then UUID,
	// including tasks without a reference from any current member.
	Tasks []APIPowerTask `json:"tasks"`
	// TasksError describes a list read failure, not a failed power operation.
	TasksError *APIPowerReadError `json:"tasksError"`
}

// APIRackComponentPowerState is one current inventory member, which can be an unsupported type.
type APIRackComponentPowerState struct {
	// ID is the external inventory ID, or nil until linked or when omitted.
	ID *string `json:"id"`
	// Type retains unsupported inventory types; unknown and unrecognized types use `Unknown`.
	Type string `json:"type"`
	// Actual is this member's live observation or read error.
	Actual APIPowerObservation `json:"actual"`
	// Desired is this member's stored policy or read error.
	Desired APIPowerPolicy `json:"desired"`
	// Operations keeps this member's Core actions and references to root Flow tasks.
	Operations APIRackComponentPowerOperations `json:"operations"`
}

// APIRackComponentPowerOperations refers to full task records in the enclosing rack's operations.
type APIRackComponentPowerOperations struct {
	// Actions is an explicit array, ordered `Active` before `Pending`.
	Actions []APIPowerAction `json:"actions"`
	// ActionsError is nil for successful empty actions and `Unsupported` for missing service data.
	ActionsError *APIPowerReadError `json:"actionsError"`
	// Tasks contains this member's references in root task order, or an empty array on error.
	Tasks []APIPowerTaskReference `json:"tasks"`
	// TasksError is nil for successful empty references and `Unsupported` for missing service data.
	TasksError *APIPowerReadError `json:"tasksError"`
}

// APIPowerTaskReference identifies a root task and its relevance to one member.
type APIPowerTaskReference struct {
	// ID is the public Flow task UUID matching a root task.
	ID string `json:"id"`
	// Relevance is `Selected`, `RackAffected`, or `Unknown` for an unrecognized value.
	Relevance APIPowerTaskRelevance `json:"relevance"`
}

// FromProto copies summary decisions and list ordering without substituting internal hardware IDs.
func (d *APIRackPowerState) FromProto(p *flowv1.RackPowerDetails) {
	*d = APIRackPowerState{Components: []APIRackComponentPowerState{}}
	if p.GetRackExternalId() != "" {
		d.ID = cutil.GetPtr(p.GetRackExternalId())
	}
	d.Actual.FromProto(p.GetActual())
	d.Desired.FromProto(p.GetDesired())

	d.Operations.FromProto(p.GetOperations())
	for _, member := range p.GetTrays() {
		component := APIRackComponentPowerState{}
		component.FromProto(member)
		d.Components = append(d.Components, component)
	}
}

// FromProto preserves Flow's full task list and its read error.
// Each component already includes its actions, so REST does not read Flow's compiled copy.
func (d *APIRackPowerOperations) FromProto(p *flowv1.RackPowerOperations) {
	rootTasks := APIPowerOperations{}
	rootTasks.tasksFromProto(p.GetTasks())
	*d = APIRackPowerOperations{Tasks: rootTasks.Tasks, TasksError: rootTasks.TasksError}
}

// FromProto keeps live evidence, stored policy and outstanding work independent.
func (d *APIRackComponentPowerState) FromProto(p *flowv1.RackTrayPowerDetails) {
	*d = APIRackComponentPowerState{Type: enumOr(ProtoToAPIRackComponentTypeName, p.GetComponentType(), "Unknown")}
	if p != nil {
		d.ID = p.ComponentId
	}
	d.Actual.FromProto(p.GetActual())
	d.Desired.FromProto(p.GetDesired())
	d.Operations.FromProto(p.GetOperations())
}

// FromProto preserves successful empty references and marks omitted service data unsupported.
func (d *APIRackComponentPowerOperations) FromProto(p *flowv1.RackTrayPowerOperations) {
	actions := APIPowerOperations{}
	actions.actionsFromProto(p.GetActions())
	*d = APIRackComponentPowerOperations{
		Actions: actions.Actions, ActionsError: actions.ActionsError,
		Tasks: []APIPowerTaskReference{}, TasksError: cutil.GetPtr[APIPowerReadError]("Unsupported"),
	}
	references := p.GetTasks()
	if references == nil {
		return
	}
	d.TasksError = nil
	if references.Error != nil {
		d.TasksError = new(APIPowerReadError)
		d.TasksError.FromProto(*references.Error)
		return
	}
	for _, reference := range references.Entries {
		entry := APIPowerTaskReference{}
		entry.FromProto(reference)
		d.Tasks = append(d.Tasks, entry)
	}
}

// FromProto retains the public task UUID and this member's supplied relevance.
func (d *APIPowerTaskReference) FromProto(p *flowv1.PowerTaskReference) {
	*d = APIPowerTaskReference{ID: p.GetId().GetId()}
	d.Relevance.FromProto(p.GetRelevance())
}

// FromProto retains Flow's summary rather than deriving votes from the REST representation.
func (d *APIRackPowerSummary) FromProto(p *flowv1.RackPowerSummary) {
	*d = APIRackPowerSummary{Complete: p.GetComplete()}
	d.State.FromProto(p.GetState())
	d.Coverage.FromProto(p.GetCoverage())
}

// FromProto preserves Flow's mutually exclusive counts without reclassifying REST errors.
func (d *APIRackPowerCoverage) FromProto(p *flowv1.RackPowerCoverage) {
	*d = APIRackPowerCoverage{
		Total: p.GetTotal(), Known: p.GetKnown(), Unknown: p.GetUnknown(),
		Unavailable: p.GetUnavailable(), NotApplicable: p.GetNotApplicable(),
		NoPolicy: p.GetNoPolicy(), DisabledPolicy: p.GetDisabledPolicy(), Unsupported: p.GetUnsupported(),
	}
}

// FromProto preserves public state spellings and maps an unrecognized value to `Unknown`.
func (s *APIRackPowerSummaryState) FromProto(state flowv1.RackPowerState) {
	*s = enumOr(rackPowerStateNames, state, "Unknown")
}
