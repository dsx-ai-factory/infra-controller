// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"fmt"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	validationis "github.com/go-ozzo/ozzo-validation/v4/is"

	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
)

const (
	PowerControlStateOn         = "on"
	PowerControlStateOff        = "off"
	PowerControlStateCycle      = "cycle"
	PowerControlStateForceOff   = "forceoff"
	PowerControlStateForceCycle = "forcecycle"
	PowerControlStateACCycle    = "acpowercycle"
)

// ValidPowerControlStates defines the valid states for power control operations
var ValidPowerControlStates = []string{
	PowerControlStateOn,
	PowerControlStateOff,
	PowerControlStateCycle,
	PowerControlStateForceOff,
	PowerControlStateForceCycle,
	PowerControlStateACCycle,
}

var validPowerControlStatesAny = func() []interface{} {
	result := make([]interface{}, len(ValidPowerControlStates))
	for i, s := range ValidPowerControlStates {
		result[i] = s
	}
	return result
}()

// ========== Power Control Request ==========

// APIUpdatePowerStateRequest is the request body for power control operations
type APIUpdatePowerStateRequest struct {
	SiteID string `json:"siteId"`
	State  string `json:"state"`
	// RuleID, when set, overrides the default rule resolution and pins this
	// operation to the named Operation Rule. Must be a valid UUID belonging
	// to the same Site and matching the operation's type/code.
	RuleID *string `json:"ruleId"`
	// OverrideReadinessCheck, when true, proceeds with the operation even if
	// one or more target components (or hosts on the owning rack for
	// rack-scoped components) are reported as not ready by their persisted
	// status. Intended for operator-supervised maintenance.
	OverrideReadinessCheck bool `json:"overrideReadinessCheck,omitempty"`
}

// Validate validates the power control request
func (r *APIUpdatePowerStateRequest) Validate() error {
	return validation.ValidateStruct(r,
		validation.Field(&r.SiteID, validation.Required.Error("siteId is required")),
		validation.Field(&r.State,
			validation.Required.Error(validationErrorValueRequired),
			validation.In(validPowerControlStatesAny...).Error(
				fmt.Sprintf("must be one of %v", ValidPowerControlStates))),
		validation.Field(&r.RuleID, validationis.UUID.Error(validationErrorInvalidUUID)),
	)
}

// ========== Power Control Response ==========

// APIUpdatePowerStateResponse is the API response for power control operations
type APIUpdatePowerStateResponse struct {
	TaskIDs []string `json:"taskIds"`
}

// FromProto converts an Flow SubmitTaskResponse to an APIUpdatePowerStateResponse
func (r *APIUpdatePowerStateResponse) FromProto(resp *flowv1.SubmitTaskResponse) {
	if resp == nil {
		r.TaskIDs = []string{}
		return
	}
	r.TaskIDs = make([]string, 0, len(resp.GetTaskIds()))
	for _, id := range resp.GetTaskIds() {
		r.TaskIDs = append(r.TaskIDs, id.GetId())
	}
}

// NewAPIUpdatePowerStateResponse creates an APIUpdatePowerStateResponse from an Flow SubmitTaskResponse
func NewAPIUpdatePowerStateResponse(resp *flowv1.SubmitTaskResponse) *APIUpdatePowerStateResponse {
	r := &APIUpdatePowerStateResponse{}
	r.FromProto(resp)
	return r
}

// ========== Batch Update Rack Power State Request ==========

// APIBatchUpdateRackPowerStateRequest is the JSON body for batch rack power control.
type APIBatchUpdateRackPowerStateRequest struct {
	SiteID string      `json:"siteId"`
	Filter *RackFilter `json:"filter,omitempty"`
	State  string      `json:"state"`
	// RuleID, when set, pins every task spawned by this batch to the named
	// Operation Rule. See APIUpdatePowerStateRequest.RuleID for semantics.
	RuleID *string `json:"ruleId"`
	// OverrideReadinessCheck applies the readiness-gate bypass to every task
	// spawned by this batch. See APIUpdatePowerStateRequest for semantics.
	OverrideReadinessCheck bool `json:"overrideReadinessCheck,omitempty"`
}

// Validate checks required fields and power state validity.
func (r *APIBatchUpdateRackPowerStateRequest) Validate() error {
	if r.SiteID == "" {
		return fmt.Errorf("siteId is required")
	}
	return validation.ValidateStruct(r,
		validation.Field(&r.State,
			validation.Required.Error(validationErrorValueRequired),
			validation.In(validPowerControlStatesAny...).Error(
				fmt.Sprintf("must be one of %v", ValidPowerControlStates))),
		validation.Field(&r.RuleID, validationis.UUID.Error(validationErrorInvalidUUID)),
	)
}

// ========== Batch Update Tray Power State Request ==========

// APIBatchUpdateTrayPowerStateRequest is the JSON body for batch tray power control.
type APIBatchUpdateTrayPowerStateRequest struct {
	SiteID string      `json:"siteId"`
	Filter *TrayFilter `json:"filter,omitempty"`
	State  string      `json:"state"`
	// RuleID, when set, pins every task spawned by this batch to the named
	// Operation Rule. See APIUpdatePowerStateRequest.RuleID for semantics.
	RuleID *string `json:"ruleId"`
	// OverrideReadinessCheck applies the readiness-gate bypass to every task
	// spawned by this batch. See APIUpdatePowerStateRequest for semantics.
	OverrideReadinessCheck bool `json:"overrideReadinessCheck,omitempty"`
}

// Validate checks required fields, power state validity, and filter constraints.
func (r *APIBatchUpdateTrayPowerStateRequest) Validate() error {
	if r.SiteID == "" {
		return fmt.Errorf("siteId is required")
	}
	if err := r.Filter.Validate(); err != nil {
		return err
	}
	return validation.ValidateStruct(r,
		validation.Field(&r.State,
			validation.Required.Error(validationErrorValueRequired),
			validation.In(validPowerControlStatesAny...).Error(
				fmt.Sprintf("must be one of %v", ValidPowerControlStates))),
		validation.Field(&r.RuleID, validationis.UUID.Error(validationErrorInvalidUUID)),
	)
}
