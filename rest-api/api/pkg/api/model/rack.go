// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"fmt"
	"net/url"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	validationis "github.com/go-ozzo/ozzo-validation/v4/is"

	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
)

// ProtoToAPIBMCTypeName maps protobuf BMCType to API-friendly names.
var ProtoToAPIBMCTypeName = map[flowv1.BMCType]string{
	flowv1.BMCType_BMC_TYPE_UNKNOWN: "BmcTypeUnknown",
	flowv1.BMCType_BMC_TYPE_HOST:    "BmcTypeHost",
	flowv1.BMCType_BMC_TYPE_DPU:     "BmcTypeDpu",
}

// ProtoToAPIRackComponentTypeName maps protobuf ComponentType to API-friendly names for rack components.
var ProtoToAPIRackComponentTypeName = map[flowv1.ComponentType]string{
	flowv1.ComponentType_COMPONENT_TYPE_UNKNOWN:    "Unknown",
	flowv1.ComponentType_COMPONENT_TYPE_COMPUTE:    "Compute",
	flowv1.ComponentType_COMPONENT_TYPE_NVSWITCH:   "NVSwitch",
	flowv1.ComponentType_COMPONENT_TYPE_POWERSHELF: "PowerShelf",
	flowv1.ComponentType_COMPONENT_TYPE_TORSWITCH:  "TORSwitch",
	flowv1.ComponentType_COMPONENT_TYPE_UMS:        "UMS",
	flowv1.ComponentType_COMPONENT_TYPE_CDU:        "CDU",
}

// ProtoToAPIDiffTypeName maps protobuf DiffType to API-friendly names.
var ProtoToAPIDiffTypeName = map[flowv1.DiffType]string{
	flowv1.DiffType_DIFF_TYPE_UNKNOWN:    "Unknown",
	flowv1.DiffType_DIFF_TYPE_MISSING:    "Missing",
	flowv1.DiffType_DIFF_TYPE_UNEXPECTED: "Unexpected",
	flowv1.DiffType_DIFF_TYPE_MISMATCH:   "Mismatch",
}

// enumOr returns mapped value or fallback when key is missing from mapping.
func enumOr[K comparable, V any](m map[K]V, key K, fallback V) V {
	if v, ok := m[key]; ok {
		return v
	}
	return fallback
}

// ========== Rack Query Fields ==========

// RackFilterFieldMap maps API field names to Flow protobuf filter enum
var RackFilterFieldMap = map[string]flowv1.RackFilterField{
	"name":         flowv1.RackFilterField_RACK_FILTER_FIELD_NAME,
	"manufacturer": flowv1.RackFilterField_RACK_FILTER_FIELD_MANUFACTURER,
	"model":        flowv1.RackFilterField_RACK_FILTER_FIELD_MODEL,
}

// RackOrderByFieldMap maps API field names to Flow protobuf order by enum
var RackOrderByFieldMap = map[string]flowv1.RackOrderByField{
	"name":         flowv1.RackOrderByField_RACK_ORDER_BY_FIELD_NAME,
	"manufacturer": flowv1.RackOrderByField_RACK_ORDER_BY_FIELD_MANUFACTURER,
	"model":        flowv1.RackOrderByField_RACK_ORDER_BY_FIELD_MODEL,
}

// GetProtoRackFilter creates an Flow protobuf filter for the given rack field and patterns.
// Multiple patterns are OR'd together.
func GetProtoRackFilter(fieldName string, patterns []string) *flowv1.Filter {
	field, ok := RackFilterFieldMap[fieldName]
	if !ok || len(patterns) == 0 {
		return nil
	}
	return &flowv1.Filter{
		Field: &flowv1.Filter_RackField{
			RackField: field,
		},
		QueryInfo: &flowv1.StringQueryInfo{
			Patterns:   patterns,
			IsWildcard: false,
			UseOr:      len(patterns) > 1,
		},
	}
}

// GetProtoRackOrderByFromQueryParam creates an Flow protobuf OrderBy from API query parameters
func GetProtoRackOrderByFromQueryParam(fieldName, direction string) *flowv1.OrderBy {
	field, ok := RackOrderByFieldMap[fieldName]
	if !ok {
		return nil
	}
	return &flowv1.OrderBy{
		Field: &flowv1.OrderBy_RackField{
			RackField: field,
		},
		Direction: direction,
	}
}

// ========== Rack Request Models ==========

// APIRackGetRequest captures query parameters for getting a single rack.
type APIRackGetRequest struct {
	SiteID            string `query:"siteId"`
	IncludeComponents bool   `query:"includeComponents"`
}

func (r *APIRackGetRequest) Validate() error {
	if r.SiteID == "" {
		return fmt.Errorf("siteId query parameter is required")
	}
	return nil
}

// ========== Rack Filter (for batch operations) ==========

// RackFilter specifies which racks to target in a batch operation.
// If nil or empty, the operation targets all racks in the site.
type RackFilter struct {
	Names []string `json:"names,omitempty"`
}

// ToTargetSpec converts the filter to an Flow OperationTargetSpec.
// Handles nil receiver gracefully (targets all racks).
func (f *RackFilter) ToTargetSpec() *flowv1.OperationTargetSpec {
	var rackTargets []*flowv1.RackTarget

	if f != nil {
		for _, name := range f.Names {
			rackTargets = append(rackTargets, &flowv1.RackTarget{
				Identifier: &flowv1.RackTarget_Name{
					Name: name,
				},
			})
		}
	}

	if len(rackTargets) == 0 {
		rackTargets = append(rackTargets, &flowv1.RackTarget{})
	}

	return &flowv1.OperationTargetSpec{
		Targets: &flowv1.OperationTargetSpec_Racks{
			Racks: &flowv1.RackTargets{
				Targets: rackTargets,
			},
		},
	}
}

// APIRackGetAllRequest captures query parameters for listing racks.
type APIRackGetAllRequest struct {
	SiteID            string   `query:"siteId"`
	IncludeComponents bool     `query:"includeComponents"`
	Name              []string `query:"name"`
	Manufacturer      []string `query:"manufacturer"`
	PageNumber        string   `query:"pageNumber"`
	PageSize          string   `query:"pageSize"`
	OrderBy           string   `query:"orderBy"`
}

func (r *APIRackGetAllRequest) Validate() error {
	if r.SiteID == "" {
		return fmt.Errorf("siteId query parameter is required")
	}
	return nil
}

// ToFilters converts the request's filter fields to Flow protobuf filters.
func (r *APIRackGetAllRequest) ToFilters() []*flowv1.Filter {
	var filters []*flowv1.Filter
	if f := GetProtoRackFilter("name", r.Name); f != nil {
		filters = append(filters, f)
	}
	if f := GetProtoRackFilter("manufacturer", r.Manufacturer); f != nil {
		filters = append(filters, f)
	}
	return filters
}

// QueryValues returns only the known query parameters as url.Values,
// suitable for deterministic workflow ID hashing without unknown param interference.
func (r *APIRackGetAllRequest) QueryValues() url.Values {
	v := url.Values{}
	v.Set("siteId", r.SiteID)
	if r.IncludeComponents {
		v.Set("includeComponents", "true")
	}
	for _, n := range r.Name {
		v.Add("name", n)
	}
	for _, m := range r.Manufacturer {
		v.Add("manufacturer", m)
	}
	if r.PageNumber != "" {
		v.Set("pageNumber", r.PageNumber)
	}
	if r.PageSize != "" {
		v.Set("pageSize", r.PageSize)
	}
	if r.OrderBy != "" {
		v.Set("orderBy", r.OrderBy)
	}
	return v
}

// APIRackValidateAllRequest captures query parameters for validating racks.
type APIRackValidateAllRequest struct {
	SiteID       string   `query:"siteId"`
	Name         []string `query:"name"`
	Manufacturer []string `query:"manufacturer"`
}

func (r *APIRackValidateAllRequest) Validate() error {
	if r.SiteID == "" {
		return fmt.Errorf("siteId query parameter is required")
	}
	return nil
}

// ToFilters converts the request's filter fields to Flow protobuf filters.
func (r *APIRackValidateAllRequest) ToFilters() []*flowv1.Filter {
	var filters []*flowv1.Filter
	if f := GetProtoRackFilter("name", r.Name); f != nil {
		filters = append(filters, f)
	}
	if f := GetProtoRackFilter("manufacturer", r.Manufacturer); f != nil {
		filters = append(filters, f)
	}
	return filters
}

// QueryValues returns only the known query parameters as url.Values.
func (r *APIRackValidateAllRequest) QueryValues() url.Values {
	v := url.Values{}
	v.Set("siteId", r.SiteID)
	for _, n := range r.Name {
		v.Add("name", n)
	}
	for _, m := range r.Manufacturer {
		v.Add("manufacturer", m)
	}
	return v
}

// ========== Rack API Models ==========

// APIRack is the API representation of a Rack from Flow
type APIRack struct {
	ID           string `json:"id"`
	Name         string `json:"name"`
	Manufacturer string `json:"manufacturer"`
	Model        string `json:"model"`
	SerialNumber string `json:"serialNumber"`
	Description  string `json:"description"`
	// NVLinkDomainIDs identifies the NVLink Domains containing the Rack.
	// It is empty when the Rack is not assigned to an NVLink Domain.
	NVLinkDomainIDs []string            `json:"nvLinkDomainIds"`
	Location        *APIRackLocation    `json:"location,omitempty"`
	Components      []*APIRackComponent `json:"components,omitempty"`
	TaskStats       APITaskStats        `json:"taskStats"`
}

// FromProto converts an Flow protobuf Rack to an APIRack
func (ar *APIRack) FromProto(protoRack *flowv1.Rack, includeComponents bool) {
	if protoRack == nil {
		return
	}

	ar.ID = protoRack.GetExternalId()
	// Get info from DeviceInfo
	if protoRack.GetInfo() != nil {
		info := protoRack.GetInfo()
		ar.Name = info.GetName()
		ar.Manufacturer = info.GetManufacturer()
		if info.Model != nil {
			ar.Model = *info.Model
		}
		ar.SerialNumber = info.GetSerialNumber()
		if info.Description != nil {
			ar.Description = *info.Description
		}
	}
	ar.NVLinkDomainIDs = make([]string, 0, len(protoRack.GetNvlDomainIds()))
	for _, domainID := range protoRack.GetNvlDomainIds() {
		if domainID != nil {
			ar.NVLinkDomainIDs = append(ar.NVLinkDomainIDs, domainID.GetId())
		}
	}
	ar.TaskStats.FromProto(protoRack.GetTaskStats())

	// Get location
	if protoRack.GetLocation() != nil {
		ar.Location = &APIRackLocation{}
		ar.Location.FromProto(protoRack.GetLocation())
	}

	// Get components
	if includeComponents && len(protoRack.GetComponents()) > 0 {
		ar.Components = make([]*APIRackComponent, 0, len(protoRack.GetComponents()))
		for _, comp := range protoRack.GetComponents() {
			apiComp := &APIRackComponent{}
			apiComp.FromProto(comp)
			ar.Components = append(ar.Components, apiComp)
		}
	}
}

// NewAPIRack creates an APIRack from the Flow protobuf Rack
func NewAPIRack(protoRack *flowv1.Rack, includeComponents bool) *APIRack {
	if protoRack == nil {
		return nil
	}
	apiRack := &APIRack{}
	apiRack.FromProto(protoRack, includeComponents)
	return apiRack
}

// APIRackLocation represents the location of a rack
type APIRackLocation struct {
	Region     string `json:"region"`
	Datacenter string `json:"datacenter"`
	Room       string `json:"room"`
	Position   string `json:"position"`
}

// FromProto converts a proto Location to an APIRackLocation
func (arl *APIRackLocation) FromProto(protoLocation *flowv1.Location) {
	if protoLocation == nil {
		return
	}
	arl.Region = protoLocation.GetRegion()
	arl.Datacenter = protoLocation.GetDatacenter()
	arl.Room = protoLocation.GetRoom()
	arl.Position = protoLocation.GetPosition()
}

// APIBMC represents a BMC (Baseboard Management Controller) entry
type APIBMC struct {
	Type       string `json:"type"`
	MacAddress string `json:"macAddress"`
	IPAddress  string `json:"ipAddress"`
}

// FromProto converts a proto BMC to an APIBMC
func (ab *APIBMC) FromProto(protoBMC *flowv1.BMCInfo) {
	if protoBMC == nil {
		return
	}
	ab.Type = enumOr(ProtoToAPIBMCTypeName, protoBMC.GetType(), "BmcTypeUnknown")
	ab.MacAddress = protoBMC.GetMacAddress()
	ab.IPAddress = protoBMC.GetIpAddress()
}

// APIRackComponent represents a component within a rack
type APIRackComponent struct {
	ID              string    `json:"id"`
	RackID          string    `json:"rackId"`
	Type            string    `json:"type"`
	Name            string    `json:"name"`
	SerialNumber    string    `json:"serialNumber"`
	Manufacturer    string    `json:"manufacturer"`
	Model           string    `json:"model"`
	Description     string    `json:"description"`
	FirmwareVersion string    `json:"firmwareVersion"`
	SlotID          int32     `json:"slotId"`
	TrayIdx         int32     `json:"trayIdx"`
	HostID          int32     `json:"hostId"`
	BMCs            []*APIBMC `json:"bmcs"`
	PowerState      string    `json:"powerState"`
	OperationStatus string    `json:"operationStatus"`
	LeakStatus      string    `json:"leakStatus"`
}

// FromProto converts a proto Component to an APIRackComponent
func (arc *APIRackComponent) FromProto(protoComponent *flowv1.Component) {
	if protoComponent == nil {
		return
	}
	arc.Type = enumOr(ProtoToAPIRackComponentTypeName, protoComponent.GetType(), "Unknown")
	arc.FirmwareVersion = protoComponent.GetFirmwareVersion()
	arc.ID = protoComponent.GetComponentId()
	arc.PowerState = protoComponent.GetPowerState()
	arc.OperationStatus = enumOr(ProtoToAPIPhaseName, protoComponent.GetStatus().GetPhase(), "Unknown")
	arc.LeakStatus = enumOr(ProtoToAPILeakStatusName, protoComponent.GetLeakStatus(), "Unknown")

	// Get rack ID
	arc.RackID = protoComponent.GetRackExternalId()

	// Get component info
	if protoComponent.GetInfo() != nil {
		compInfo := protoComponent.GetInfo()
		arc.Name = compInfo.GetName()
		arc.SerialNumber = compInfo.GetSerialNumber()
		arc.Manufacturer = compInfo.GetManufacturer()
		arc.Model = compInfo.GetModel()
		arc.Description = compInfo.GetDescription()
	}

	// Get position
	if protoComponent.GetPosition() != nil {
		arc.SlotID = protoComponent.GetPosition().GetSlotId()
		arc.TrayIdx = protoComponent.GetPosition().GetTrayIdx()
		arc.HostID = protoComponent.GetPosition().GetHostId()
	}

	// Get BMCs
	if len(protoComponent.GetBmcs()) > 0 {
		arc.BMCs = make([]*APIBMC, 0, len(protoComponent.GetBmcs()))
		for _, bmc := range protoComponent.GetBmcs() {
			apiBMC := &APIBMC{}
			apiBMC.FromProto(bmc)
			arc.BMCs = append(arc.BMCs, apiBMC)
		}
	}
}

// ========== Rack Validation API Models ==========

// APIFieldDiff represents a single field difference
type APIFieldDiff struct {
	FieldName     string `json:"fieldName"`
	ExpectedValue string `json:"expectedValue"`
	ActualValue   string `json:"actualValue"`
}

// FromProto converts an Flow protobuf FieldDiff to an APIFieldDiff
func (f *APIFieldDiff) FromProto(protoFieldDiff *flowv1.FieldDiff) {
	if protoFieldDiff == nil {
		return
	}
	f.FieldName = protoFieldDiff.GetFieldName()
	f.ExpectedValue = protoFieldDiff.GetExpectedValue()
	f.ActualValue = protoFieldDiff.GetActualValue()
}

// APIComponentDiff represents a single component difference found during validation
type APIComponentDiff struct {
	Type       string            `json:"type"`
	ID         *string           `json:"id"`
	MACAddress *string           `json:"macAddress"`
	Expected   *APIRackComponent `json:"expected,omitempty"`
	Actual     *APIRackComponent `json:"actual,omitempty"`
	FieldDiffs []*APIFieldDiff   `json:"fieldDiffs,omitempty"`
}

// FromProto converts an Flow protobuf ComponentDiff to an APIComponentDiff
func (d *APIComponentDiff) FromProto(protoDiff *flowv1.ComponentDiff) {
	if protoDiff == nil {
		return
	}

	d.Type = enumOr(ProtoToAPIDiffTypeName, protoDiff.GetType(), "Unknown")
	if componentID := protoDiff.GetComponentId(); componentID != "" {
		d.ID = &componentID
	}
	if macAddress := protoDiff.GetComponentMacAddress(); macAddress != "" {
		d.MACAddress = &macAddress
	}

	if protoDiff.GetExpected() != nil {
		d.Expected = &APIRackComponent{}
		d.Expected.FromProto(protoDiff.GetExpected())
	}

	if protoDiff.GetActual() != nil {
		d.Actual = &APIRackComponent{}
		d.Actual.FromProto(protoDiff.GetActual())
	}

	if len(protoDiff.GetFieldDiffs()) > 0 {
		d.FieldDiffs = make([]*APIFieldDiff, 0, len(protoDiff.GetFieldDiffs()))
		for _, fd := range protoDiff.GetFieldDiffs() {
			apiFieldDiff := &APIFieldDiff{}
			apiFieldDiff.FromProto(fd)
			d.FieldDiffs = append(d.FieldDiffs, apiFieldDiff)
		}
	}
}

// APIRackValidationResult is the API representation of a rack validation result
type APIRackValidationResult struct {
	Diffs           []*APIComponentDiff `json:"diffs"`
	TotalDiffs      int32               `json:"totalDiffs"`
	MissingCount    int32               `json:"missingCount"`
	UnexpectedCount int32               `json:"unexpectedCount"`
	MismatchCount   int32               `json:"mismatchCount"`
	MatchCount      int32               `json:"matchCount"`
}

// FromProto converts an Flow protobuf ValidateComponentsResponse to an APIRackValidationResult
func (r *APIRackValidationResult) FromProto(protoResp *flowv1.ValidateComponentsResponse) {
	if protoResp == nil {
		return
	}

	r.TotalDiffs = protoResp.GetTotalDiffs()
	r.MissingCount = protoResp.GetMissingCount()
	r.UnexpectedCount = protoResp.GetUnexpectedCount()
	r.MismatchCount = protoResp.GetMismatchCount()
	r.MatchCount = protoResp.GetMatchCount()

	r.Diffs = make([]*APIComponentDiff, 0, len(protoResp.GetDiffs()))
	for _, diff := range protoResp.GetDiffs() {
		apiDiff := &APIComponentDiff{}
		apiDiff.FromProto(diff)
		r.Diffs = append(r.Diffs, apiDiff)
	}
}

// NewAPIRackValidationResult creates an APIRackValidationResult from the Flow protobuf response
func NewAPIRackValidationResult(protoResp *flowv1.ValidateComponentsResponse) *APIRackValidationResult {
	if protoResp == nil {
		return nil
	}
	result := &APIRackValidationResult{}
	result.FromProto(protoResp)
	return result
}

// ========== Bring Up Request ==========

// APIBringUpRackRequest is the request body for bring up operations on a single rack
type APIBringUpRackRequest struct {
	SiteID      string `json:"siteId"`
	Description string `json:"description,omitempty"`
	// RuleID, when set, overrides the default rule resolution and pins the
	// bring-up operation to the named Operation Rule.
	RuleID *string `json:"ruleId"`
	// OverrideReadinessCheck, when true, proceeds with the bring-up even if
	// one or more target components (or hosts on the owning rack) are reported
	// as not ready by their persisted status. Intended for operator-supervised
	// maintenance.
	OverrideReadinessCheck bool `json:"overrideReadinessCheck,omitempty"`
}

// Validate validates the bring up request
func (r *APIBringUpRackRequest) Validate() error {
	return validation.ValidateStruct(r,
		validation.Field(&r.SiteID, validation.Required.Error("siteId is required")),
		validation.Field(&r.RuleID, validationis.UUID.Error(validationErrorInvalidUUID)),
	)
}

// ========== Bring Up Response ==========

// APIBringUpRackResponse is the API response for bring up operations
type APIBringUpRackResponse struct {
	TaskIDs []string `json:"taskIds"`
}

// FromProto converts an Flow SubmitTaskResponse to an APIBringUpRackResponse
func (r *APIBringUpRackResponse) FromProto(resp *flowv1.SubmitTaskResponse) {
	if resp == nil {
		r.TaskIDs = []string{}
		return
	}
	r.TaskIDs = make([]string, 0, len(resp.GetTaskIds()))
	for _, id := range resp.GetTaskIds() {
		r.TaskIDs = append(r.TaskIDs, id.GetId())
	}
}

// NewAPIBringUpRackResponse creates an APIBringUpRackResponse from an Flow SubmitTaskResponse
func NewAPIBringUpRackResponse(resp *flowv1.SubmitTaskResponse) *APIBringUpRackResponse {
	r := &APIBringUpRackResponse{}
	r.FromProto(resp)
	return r
}

// ========== Batch Bring Up Rack Request ==========

// APIBatchBringUpRackRequest is the JSON body for batch rack bring up.
type APIBatchBringUpRackRequest struct {
	SiteID      string      `json:"siteId"`
	Filter      *RackFilter `json:"filter,omitempty"`
	Description string      `json:"description,omitempty"`
	// RuleID, when set, pins every bring-up task spawned by this batch to the
	// named Operation Rule.
	RuleID *string `json:"ruleId"`
	// OverrideReadinessCheck applies the readiness-gate bypass to every task
	// spawned by this batch. See APIBringUpRackRequest for semantics.
	OverrideReadinessCheck bool `json:"overrideReadinessCheck,omitempty"`
}

// Validate checks required fields.
func (r *APIBatchBringUpRackRequest) Validate() error {
	return validation.ValidateStruct(r,
		validation.Field(&r.SiteID, validation.Required.Error("siteId is required")),
		validation.Field(&r.RuleID, validationis.UUID.Error(validationErrorInvalidUUID)),
	)
}
