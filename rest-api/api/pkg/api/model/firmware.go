// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"fmt"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	validationis "github.com/go-ozzo/ozzo-validation/v4/is"

	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
)

// ========== Firmware Update Request ==========

// APIUpdateFirmwareRequest is the request body for firmware update operations
type APIUpdateFirmwareRequest struct {
	SiteID             string                         `json:"siteId"`
	Version            *string                        `json:"version"`
	AuthenticationData *APIFirmwareAuthenticationData `json:"authenticationData"`
	// Targets, when non-empty, restricts the update to a subset of
	// firmware sub-parts within the targeted tray (e.g. ["bmc", "nvos"]
	// for switch trays). Names are lowercase. The authoritative supported
	// set per tray type is derived from the Flow service's NICo proto
	// bindings (mirroring Core's per-tray-type enums in
	// carbide-core/crates/rpc/proto/forge.proto); see
	// flow/pkg/common/firmwarecomponents for the resolution logic and
	// helpers like SupportedNICoNVSwitchNames.
	// Empty/nil means "update the default targets" for the
	// compute-tray-internal targets. When non-empty, requires Version.
	//
	// On compute trays, the special target "dpu" requests DPU
	// reprovisioning on each matched host. Unlike every other target,
	// "dpu" is NOT part of the empty/nil "default targets" set; the
	// caller has to list it explicitly. Version is ignored on the
	// "dpu" branch; the target firmware version comes from site
	// configuration.
	//
	// REST surface intentionally calls these "targets" to avoid confusion
	// with carbide's tray-level "Component" vocabulary; the downstream
	// Flow proto field is named `sub_targets` and represents the same
	// enum subset.
	Targets []string `json:"targets"`
	// RuleID, when set, overrides the default rule resolution and pins this
	// firmware operation to the named Operation Rule.
	RuleID *string `json:"ruleId"`
	// OverrideReadinessCheck, when true, proceeds with the firmware update
	// even if one or more target components (or hosts on the owning rack for
	// rack-scoped components) are reported as not ready by their persisted
	// status. Intended for operator-supervised maintenance.
	OverrideReadinessCheck bool `json:"overrideReadinessCheck,omitempty"`
	// OverrideVersionCheck requests that the component backend apply firmware
	// without enforcing version-based skip or downgrade decisions.
	OverrideVersionCheck bool `json:"overrideVersionCheck"`
}

// Validate validates the firmware update request
func (r *APIUpdateFirmwareRequest) Validate() error {
	err := validation.ValidateStruct(r,
		validation.Field(&r.SiteID, validation.Required.Error("siteId is required")),
		validation.Field(&r.RuleID, validationis.UUID.Error(validationErrorInvalidUUID)),
		validation.Field(&r.AuthenticationData),
	)
	if err != nil {
		return err
	}
	return validateFirmwareTargets(r.Targets, r.Version)
}

// APIFirmwareAuthenticationData selects one shared firmware download
// credential or credentials scoped to supported tray types.
type APIFirmwareAuthenticationData struct {
	Shared       *string                                    `json:"shared"`
	PerComponent *APIPerComponentFirmwareAuthenticationData `json:"perComponent"`
}

// Validate requires exactly one authentication-data representation.
func (a APIFirmwareAuthenticationData) Validate() error {
	if (a.Shared == nil) == (a.PerComponent == nil) {
		return fmt.Errorf("must set exactly one of shared or perComponent")
	}
	return nil
}

// ToProto maps the explicit REST shape to Flow's authentication-data oneof.
func (a *APIFirmwareAuthenticationData) ToProto() *flowv1.FirmwareAuthenticationData {
	if a == nil {
		return nil
	}
	if a.Shared != nil {
		return &flowv1.FirmwareAuthenticationData{
			Value: &flowv1.FirmwareAuthenticationData_Shared{Shared: *a.Shared},
		}
	}
	return &flowv1.FirmwareAuthenticationData{
		Value: &flowv1.FirmwareAuthenticationData_PerComponent{
			PerComponent: a.PerComponent.ToProto(),
		},
	}
}

// APIPerComponentFirmwareAuthenticationData carries independent credentials
// for each supported firmware tray type. Omitted values mean no credential for
// that type.
type APIPerComponentFirmwareAuthenticationData struct {
	Compute    *string `json:"compute"`
	NVSwitch   *string `json:"nvswitch"`
	PowerShelf *string `json:"powershelf"`
}

// ToProto preserves whether each optional per-component value was provided.
func (a *APIPerComponentFirmwareAuthenticationData) ToProto() *flowv1.PerComponentFirmwareAuthenticationData {
	if a == nil {
		return nil
	}
	return &flowv1.PerComponentFirmwareAuthenticationData{
		Compute:    a.Compute,
		Nvswitch:   a.NVSwitch,
		Powershelf: a.PowerShelf,
	}
}

// ========== Firmware Update Response ==========

// APIUpdateFirmwareResponse is the API response for firmware update operations
type APIUpdateFirmwareResponse struct {
	TaskIDs []string `json:"taskIds"`
}

// FromProto converts an Flow SubmitTaskResponse to an APIUpdateFirmwareResponse
func (r *APIUpdateFirmwareResponse) FromProto(resp *flowv1.SubmitTaskResponse) {
	if resp == nil {
		r.TaskIDs = []string{}
		return
	}
	r.TaskIDs = make([]string, 0, len(resp.GetTaskIds()))
	for _, id := range resp.GetTaskIds() {
		r.TaskIDs = append(r.TaskIDs, id.GetId())
	}
}

// NewAPIUpdateFirmwareResponse creates an APIUpdateFirmwareResponse from an Flow SubmitTaskResponse
func NewAPIUpdateFirmwareResponse(resp *flowv1.SubmitTaskResponse) *APIUpdateFirmwareResponse {
	r := &APIUpdateFirmwareResponse{}
	r.FromProto(resp)
	return r
}

// ========== Batch Rack Firmware Update Request ==========

// APIBatchRackFirmwareUpdateRequest is the JSON body for batch rack firmware update.
type APIBatchRackFirmwareUpdateRequest struct {
	SiteID             string                         `json:"siteId"`
	Filter             *RackFilter                    `json:"filter"`
	Version            *string                        `json:"version"`
	AuthenticationData *APIFirmwareAuthenticationData `json:"authenticationData"`
	// RuleID, when set, pins every task spawned by this batch to the named
	// Operation Rule.
	RuleID *string `json:"ruleId"`
	// OverrideReadinessCheck applies the readiness-gate bypass to every task
	// spawned by this batch. See APIUpdateFirmwareRequest for semantics.
	OverrideReadinessCheck bool `json:"overrideReadinessCheck,omitempty"`
	// OverrideVersionCheck applies the firmware version-check override to every
	// task spawned by this batch. See APIUpdateFirmwareRequest for semantics.
	OverrideVersionCheck bool `json:"overrideVersionCheck"`
}

// Validate checks required fields.
func (r *APIBatchRackFirmwareUpdateRequest) Validate() error {
	return validation.ValidateStruct(r,
		validation.Field(&r.SiteID, validation.Required.Error("siteId is required")),
		validation.Field(&r.RuleID, validationis.UUID.Error(validationErrorInvalidUUID)),
		validation.Field(&r.AuthenticationData),
	)
}

// ========== Batch Tray Firmware Update Request ==========

// APIBatchTrayFirmwareUpdateRequest is the JSON body for batch tray firmware update.
type APIBatchTrayFirmwareUpdateRequest struct {
	SiteID             string                         `json:"siteId"`
	Filter             *TrayFilter                    `json:"filter"`
	Version            *string                        `json:"version"`
	AuthenticationData *APIFirmwareAuthenticationData `json:"authenticationData"`
	// Targets, when non-empty, restricts the update to a subset of
	// firmware sub-parts within each matched tray. Same semantics as the
	// single-tray variant. When non-empty, requires Version.
	Targets []string `json:"targets"`
	// RuleID, when set, pins every task spawned by this batch to the named
	// Operation Rule.
	RuleID *string `json:"ruleId"`
	// OverrideReadinessCheck applies the readiness-gate bypass to every task
	// spawned by this batch. See APIUpdateFirmwareRequest for semantics.
	OverrideReadinessCheck bool `json:"overrideReadinessCheck,omitempty"`
	// OverrideVersionCheck applies the firmware version-check override to every
	// task spawned by this batch. See APIUpdateFirmwareRequest for semantics.
	OverrideVersionCheck bool `json:"overrideVersionCheck"`
}

// Validate checks required fields and filter constraints.
func (r *APIBatchTrayFirmwareUpdateRequest) Validate() error {
	err := validation.ValidateStruct(r,
		validation.Field(&r.SiteID, validation.Required.Error("siteId is required")),
		validation.Field(&r.RuleID, validationis.UUID.Error(validationErrorInvalidUUID)),
		validation.Field(&r.AuthenticationData),
	)
	if err != nil {
		return err
	}
	if r.Filter != nil {
		if err := r.Filter.Validate(); err != nil {
			return err
		}
	}
	return validateFirmwareTargets(r.Targets, r.Version)
}

// validateFirmwareTargets enforces the cross-field constraint that a
// firmware-target subset selection is only meaningful when a target version
// is also supplied. Per-tray-type name validation is delegated to Flow,
// where the mapping from string to component-manager enum lives.
func validateFirmwareTargets(targets []string, version *string) error {
	if len(targets) == 0 {
		return nil
	}
	for _, t := range targets {
		if t == "" {
			return fmt.Errorf("targets must not contain empty strings")
		}
	}
	if version == nil || *version == "" {
		return fmt.Errorf("targets requires version to be set")
	}
	return nil
}
