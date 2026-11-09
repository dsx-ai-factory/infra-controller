// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"encoding/json"
	"fmt"
	"sort"
	"strings"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	validationis "github.com/go-ozzo/ozzo-validation/v4/is"

	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
)

// ========== Firmware Update Request ==========

// APIUpdateFirmwareRequest is the request body for firmware update operations
type APIUpdateFirmwareRequest struct {
	SiteID  string  `json:"siteId"`
	Version *string `json:"version"`
	// AuthenticationData is the deprecated alias of FirmwareDownloadAuth.
	AuthenticationData *APIFirmwareAuthenticationData `json:"authenticationData"`
	// FirmwareDownloadAuth authenticates artifact downloads; it is mutually exclusive with AuthenticationData.
	FirmwareDownloadAuth *APIFirmwareAuthenticationData `json:"firmwareDownloadAuth"`
	// DeviceCredentialOverrides supplies device logins for direct tray firmware dispatch.
	DeviceCredentialOverrides *APIDeviceCredentialOverrides `json:"deviceCredentialOverrides"`
	// Targets, when non-empty, restricts the update to a subset of
	// firmware sub-parts within the targeted tray (e.g. ["bmc", "nvos"]
	// for switch trays). Names are lowercase. The authoritative supported
	// set per tray type is derived from the Flow service's NICo proto
	// bindings (mirroring Core's per-tray-type enums in
	// carbide-core/crates/rpc/proto/forge.proto); see
	// flow/pkg/common/firmwarecomponents for the resolution logic and
	// helpers like SupportedNICoNVSwitchNames.
	// Empty/nil means "update the default targets" for the
	// compute-tray-internal targets.
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
		validation.Field(&r.AuthenticationData, validation.By(r.validateFirmwareAuth)),
		validation.Field(&r.FirmwareDownloadAuth),
		validation.Field(&r.DeviceCredentialOverrides),
	)
	if err != nil {
		return err
	}
	return validateFirmwareTargets(r.Targets)
}

// APIFirmwareAuthenticationData selects one shared firmware download
// credential or credentials scoped to supported tray types.
type APIFirmwareAuthenticationData struct {
	Shared       *string                                    `json:"shared"`
	PerComponent *APIPerComponentFirmwareAuthenticationData `json:"perComponent"`
}

// UnknownFirmwareAuthenticationFieldError identifies fields forbidden by the
// closed firmware authentication-data schemas.
type UnknownFirmwareAuthenticationFieldError struct {
	Path   string
	Fields []string
}

// Error returns a credential-free description suitable for a client response.
func (e *UnknownFirmwareAuthenticationFieldError) Error() string {
	quotedFields := make([]string, 0, len(e.Fields))
	for _, field := range e.Fields {
		quotedFields = append(quotedFields, fmt.Sprintf("%q", field))
	}
	if len(quotedFields) == 1 {
		return fmt.Sprintf("%s contains unknown field %s", e.Path, quotedFields[0])
	}
	return fmt.Sprintf("%s contains unknown fields: %s", e.Path, strings.Join(quotedFields, ", "))
}

// UnmarshalJSON rejects fields outside the public authentication-data schema.
func (a *APIFirmwareAuthenticationData) UnmarshalJSON(data []byte) error {
	if err := rejectUnknownFirmwareAuthenticationFields(
		data,
		"authenticationData",
		"shared",
		"perComponent",
	); err != nil {
		return err
	}

	type firmwareAuthenticationData APIFirmwareAuthenticationData
	return json.Unmarshal(data, (*firmwareAuthenticationData)(a))
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

// UnmarshalJSON rejects fields outside the supported firmware tray types.
func (a *APIPerComponentFirmwareAuthenticationData) UnmarshalJSON(data []byte) error {
	if err := rejectUnknownFirmwareAuthenticationFields(
		data,
		"authenticationData.perComponent",
		"compute",
		"nvswitch",
		"powershelf",
	); err != nil {
		return err
	}

	type perComponentFirmwareAuthenticationData APIPerComponentFirmwareAuthenticationData
	return json.Unmarshal(data, (*perComponentFirmwareAuthenticationData)(a))
}

func rejectUnknownFirmwareAuthenticationFields(data []byte, path string, knownFields ...string) error {
	var fields map[string]json.RawMessage
	if err := json.Unmarshal(data, &fields); err != nil {
		return err
	}

	known := make(map[string]struct{}, len(knownFields))
	for _, field := range knownFields {
		known[field] = struct{}{}
	}

	unknown := make([]string, 0)
	for field := range fields {
		if _, ok := known[field]; !ok {
			unknown = append(unknown, field)
		}
	}
	if len(unknown) == 0 {
		return nil
	}
	sort.Strings(unknown)
	return &UnknownFirmwareAuthenticationFieldError{Path: path, Fields: unknown}
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
	SiteID  string      `json:"siteId"`
	Filter  *RackFilter `json:"filter"`
	Version *string     `json:"version"`
	// AuthenticationData is the deprecated alias of FirmwareDownloadAuth.
	AuthenticationData *APIFirmwareAuthenticationData `json:"authenticationData"`
	// FirmwareDownloadAuth authenticates artifact downloads; it is mutually exclusive with AuthenticationData.
	FirmwareDownloadAuth *APIFirmwareAuthenticationData `json:"firmwareDownloadAuth"`
	// DeviceCredentialOverrides supplies device logins for direct tray firmware dispatch.
	DeviceCredentialOverrides *APIDeviceCredentialOverrides `json:"deviceCredentialOverrides"`
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
		validation.Field(&r.AuthenticationData, validation.By(r.validateFirmwareAuth)),
		validation.Field(&r.FirmwareDownloadAuth),
		validation.Field(&r.DeviceCredentialOverrides),
	)
}

// ========== Batch Tray Firmware Update Request ==========

// APIBatchTrayFirmwareUpdateRequest is the JSON body for batch tray firmware update.
type APIBatchTrayFirmwareUpdateRequest struct {
	SiteID  string      `json:"siteId"`
	Filter  *TrayFilter `json:"filter"`
	Version *string     `json:"version"`
	// AuthenticationData is the deprecated alias of FirmwareDownloadAuth.
	AuthenticationData *APIFirmwareAuthenticationData `json:"authenticationData"`
	// FirmwareDownloadAuth authenticates artifact downloads; it is mutually exclusive with AuthenticationData.
	FirmwareDownloadAuth *APIFirmwareAuthenticationData `json:"firmwareDownloadAuth"`
	// DeviceCredentialOverrides supplies device logins for direct tray firmware dispatch.
	DeviceCredentialOverrides *APIDeviceCredentialOverrides `json:"deviceCredentialOverrides"`
	// Targets, when non-empty, restricts the update to a subset of
	// firmware sub-parts within each matched tray. Same semantics as the
	// single-tray variant.
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
		validation.Field(&r.AuthenticationData, validation.By(r.validateFirmwareAuth)),
		validation.Field(&r.FirmwareDownloadAuth),
		validation.Field(&r.DeviceCredentialOverrides),
	)
	if err != nil {
		return err
	}
	if r.Filter != nil {
		if err := r.Filter.Validate(); err != nil {
			return err
		}
	}
	return validateFirmwareTargets(r.Targets)
}

// validateFirmwareTargets rejects empty target names. Per-tray-type name
// validation is delegated to Flow, where the mapping from string to
// component-manager enum lives.
func validateFirmwareTargets(targets []string) error {
	if len(targets) == 0 {
		return nil
	}
	for _, t := range targets {
		if t == "" {
			return fmt.Errorf("targets must not contain empty strings")
		}
	}
	return nil
}

// APIDeviceCredentialOverrides supplies complete BMC and/or NVOS login pairs.
// Omitted pairs retain the configured credential lookup.
type APIDeviceCredentialOverrides struct {
	BMC  *APIDeviceLogin `json:"bmc"`
	NVOS *APIDeviceLogin `json:"nvos"`
}

// APIDeviceLogin is a write-only device username/password pair.
type APIDeviceLogin struct {
	Username string `json:"username"`
	Password string `json:"password"`
}

// Validate requires both nonempty login fields without including their values in errors.
func (a APIDeviceLogin) Validate() error {
	return validation.ValidateStruct(&a,
		validation.Field(&a.Username, validation.Required),
		validation.Field(&a.Password, validation.Required))
}

// Validate requires at least one complete device login pair.
func (a APIDeviceCredentialOverrides) Validate() error {
	err := validation.ValidateStruct(&a, validation.Field(&a.BMC), validation.Field(&a.NVOS))
	if err != nil {
		return err
	}
	if a.BMC == nil && a.NVOS == nil {
		return validation.Errors{"bmc": fmt.Errorf("bmc or nvos is required")}
	}
	return nil
}

// ToProto preserves which device login pairs were explicitly supplied.
func (a *APIDeviceCredentialOverrides) ToProto() *flowv1.DeviceCredentialOverrides {
	if a == nil {
		return nil
	}
	out := &flowv1.DeviceCredentialOverrides{}
	if a.BMC != nil {
		out.Bmc = &flowv1.DeviceLogin{Username: a.BMC.Username, Password: a.BMC.Password}
	}
	if a.NVOS != nil {
		out.Nvos = &flowv1.DeviceLogin{Username: a.NVOS.Username, Password: a.NVOS.Password}
	}
	return out
}

func (r *APIUpdateFirmwareRequest) validateFirmwareAuth(_ interface{}) error {
	if r.AuthenticationData != nil && r.FirmwareDownloadAuth != nil {
		return fmt.Errorf("authenticationData and firmwareDownloadAuth are mutually exclusive")
	}
	if r.DeviceCredentialOverrides != nil && !r.OverrideReadinessCheck {
		return fmt.Errorf("deviceCredentialOverrides requires overrideReadinessCheck")
	}
	return nil
}

// DownloadAuth returns the supplied download authentication after alias validation.
func (r *APIUpdateFirmwareRequest) DownloadAuth() *flowv1.FirmwareAuthenticationData {
	if r.FirmwareDownloadAuth != nil {
		return r.FirmwareDownloadAuth.ToProto()
	}
	return r.AuthenticationData.ToProto()
}

func (r *APIBatchRackFirmwareUpdateRequest) validateFirmwareAuth(_ interface{}) error {
	if r.AuthenticationData != nil && r.FirmwareDownloadAuth != nil {
		return fmt.Errorf("authenticationData and firmwareDownloadAuth are mutually exclusive")
	}
	if r.DeviceCredentialOverrides != nil && !r.OverrideReadinessCheck {
		return fmt.Errorf("deviceCredentialOverrides requires overrideReadinessCheck")
	}
	return nil
}

// DownloadAuth returns the supplied download authentication after alias validation.
func (r *APIBatchRackFirmwareUpdateRequest) DownloadAuth() *flowv1.FirmwareAuthenticationData {
	if r.FirmwareDownloadAuth != nil {
		return r.FirmwareDownloadAuth.ToProto()
	}
	return r.AuthenticationData.ToProto()
}

func (r *APIBatchTrayFirmwareUpdateRequest) validateFirmwareAuth(_ interface{}) error {
	if r.AuthenticationData != nil && r.FirmwareDownloadAuth != nil {
		return fmt.Errorf("authenticationData and firmwareDownloadAuth are mutually exclusive")
	}
	if r.DeviceCredentialOverrides != nil && !r.OverrideReadinessCheck {
		return fmt.Errorf("deviceCredentialOverrides requires overrideReadinessCheck")
	}
	return nil
}

// DownloadAuth returns the supplied download authentication after alias validation.
func (r *APIBatchTrayFirmwareUpdateRequest) DownloadAuth() *flowv1.FirmwareAuthenticationData {
	if r.FirmwareDownloadAuth != nil {
		return r.FirmwareDownloadAuth.ToProto()
	}
	return r.AuthenticationData.ToProto()
}

// UnmarshalJSON rejects unsupported device credential fields.
func (a *APIDeviceCredentialOverrides) UnmarshalJSON(data []byte) error {
	err := rejectUnknownFirmwareAuthenticationFields(data, "deviceCredentialOverrides", "bmc", "nvos")
	if err != nil {
		return err
	}
	type plain APIDeviceCredentialOverrides
	return json.Unmarshal(data, (*plain)(a))
}

// UnmarshalJSON rejects fields outside the explicit device login contract.
func (a *APIDeviceLogin) UnmarshalJSON(data []byte) error {
	err := rejectUnknownFirmwareAuthenticationFields(data, "device login", "username", "password")
	if err != nil {
		return err
	}
	type plain APIDeviceLogin
	return json.Unmarshal(data, (*plain)(a))
}
