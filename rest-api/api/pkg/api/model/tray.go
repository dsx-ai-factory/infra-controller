// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"fmt"
	"maps"
	"net/url"
	"slices"
	"strconv"

	validation "github.com/go-ozzo/ozzo-validation/v4"

	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
)

// APIToProtoComponentTypeName maps API tray type strings to protobuf ComponentType enum names.
// These names match Flow's internal ComponentTypeFromString (case-insensitive).
var APIToProtoComponentTypeName = map[string]string{
	"Compute":    "COMPONENT_TYPE_COMPUTE",
	"NVSwitch":   "COMPONENT_TYPE_NVSWITCH",
	"PowerShelf": "COMPONENT_TYPE_POWERSHELF",
}

// ProtoToAPIComponentTypeName maps protobuf ComponentType to API tray type strings.
var ProtoToAPIComponentTypeName = map[flowv1.ComponentType]string{
	flowv1.ComponentType_COMPONENT_TYPE_UNKNOWN:    "Unknown",
	flowv1.ComponentType_COMPONENT_TYPE_COMPUTE:    "Compute",
	flowv1.ComponentType_COMPONENT_TYPE_NVSWITCH:   "NVSwitch",
	flowv1.ComponentType_COMPONENT_TYPE_POWERSHELF: "PowerShelf",
}

// ProtoToAPIPhaseName maps a component's protobuf operation-status Phase to the
// string surfaced as `operationStatus`. A component with no computed status
// (nil) resolves to "Unknown" via enumOr.
var ProtoToAPIPhaseName = map[flowv1.Phase]string{
	flowv1.Phase_PHASE_UNKNOWN:      "Unknown",
	flowv1.Phase_PHASE_INITIALIZING: "Initializing",
	flowv1.Phase_PHASE_READY:        "Ready",
	flowv1.Phase_PHASE_IN_USE:       "InUse",
	flowv1.Phase_PHASE_ERROR:        "Error",
	flowv1.Phase_PHASE_DELETING:     "Deleting",
}

// ProtoToAPILeakStatusName maps Flow's leak-detection enum to the leak status
// surfaced as `leakStatus`. Callers care whether a tray is considered leaking,
// not about the underlying detection signal, so a fired detection maps to
// "Leaking" and a clear detection to "NoLeak".
var ProtoToAPILeakStatusName = map[flowv1.LeakStatus]string{
	flowv1.LeakStatus_LEAK_STATUS_UNKNOWN:      "Unknown",
	flowv1.LeakStatus_LEAK_STATUS_DETECTED:     "Leaking",
	flowv1.LeakStatus_LEAK_STATUS_NOT_DETECTED: "NoLeak",
}

var validTrayTypesAny, ValidProtoComponentTypes = func() ([]interface{}, []flowv1.ComponentType) {
	anyTypes := make([]interface{}, 0, len(APIToProtoComponentTypeName))
	protoTypes := make([]flowv1.ComponentType, 0, len(APIToProtoComponentTypeName))
	for apiName, protoName := range APIToProtoComponentTypeName {
		anyTypes = append(anyTypes, apiName)
		protoTypes = append(protoTypes, flowv1.ComponentType(flowv1.ComponentType_value[protoName]))
	}
	return anyTypes, protoTypes
}()

// TrayFilterFieldMap maps API field names to Flow protobuf ComponentFilterField enum for tray validation queries
var TrayFilterFieldMap = map[string]flowv1.ComponentFilterField{
	"name":         flowv1.ComponentFilterField_COMPONENT_FILTER_FIELD_NAME,
	"manufacturer": flowv1.ComponentFilterField_COMPONENT_FILTER_FIELD_MANUFACTURER,
	"type":         flowv1.ComponentFilterField_COMPONENT_FILTER_FIELD_TYPE,
}

// GetProtoTrayFilter creates an Flow protobuf Filter for the given tray field and patterns.
// Multiple patterns are OR'd together.
func GetProtoTrayFilter(fieldName string, patterns []string) *flowv1.Filter {
	field, ok := TrayFilterFieldMap[fieldName]
	if !ok || len(patterns) == 0 {
		return nil
	}
	return &flowv1.Filter{
		Field: &flowv1.Filter_ComponentField{
			ComponentField: field,
		},
		QueryInfo: &flowv1.StringQueryInfo{
			Patterns:   patterns,
			IsWildcard: false,
			UseOr:      len(patterns) > 1,
		},
	}
}

// TrayOrderByFieldMap maps API field names to Flow protobuf ComponentOrderByField enum
var TrayOrderByFieldMap = map[string]flowv1.ComponentOrderByField{
	"name":         flowv1.ComponentOrderByField_COMPONENT_ORDER_BY_FIELD_NAME,
	"manufacturer": flowv1.ComponentOrderByField_COMPONENT_ORDER_BY_FIELD_MANUFACTURER,
	"model":        flowv1.ComponentOrderByField_COMPONENT_ORDER_BY_FIELD_MODEL,
	"type":         flowv1.ComponentOrderByField_COMPONENT_ORDER_BY_FIELD_TYPE,
}

// GetProtoTrayOrderByFromQueryParam creates an Flow protobuf OrderBy from API query parameters for tray (component) queries
func GetProtoTrayOrderByFromQueryParam(fieldName, direction string) *flowv1.OrderBy {
	field, ok := TrayOrderByFieldMap[fieldName]
	if !ok {
		return nil
	}
	return &flowv1.OrderBy{
		Field: &flowv1.OrderBy_ComponentField{
			ComponentField: field,
		},
		Direction: direction,
	}
}

// RackComponentSlotMatcher tests whether a component sits at the requested rack slot.
type RackComponentSlotMatcher struct {
	SlotID *int32
}

func (m RackComponentSlotMatcher) Active() bool {
	return m.SlotID != nil
}

// Matches reports whether comp sits at the matcher's slot when Active.
func (m RackComponentSlotMatcher) Matches(comp *flowv1.Component) bool {
	if !m.Active() {
		return true
	}
	if comp == nil {
		return false
	}
	pos := comp.GetPosition()
	if pos == nil {
		return false
	}
	return pos.GetSlotId() == *m.SlotID
}

func validateSlotRequiresRack(slotID *int32, rackID, rackName *string) error {
	if slotID != nil && rackID == nil && rackName == nil {
		return validation.Errors{"slotId": fmt.Errorf("rackId or rackName is required when slotId is set")}
	}
	return nil
}

func validateSlotConstraints(slotID *int32, rackID, rackName *string) error {
	if slotID == nil {
		return nil
	}
	if err := validation.Validate(*slotID, validation.Min(int32(0)).Error("must be >= 0")); err != nil {
		return validation.Errors{"slotId": err}
	}
	return validateSlotRequiresRack(slotID, rackID, rackName)
}

// ========== Tray Filter (for batch operations) ==========

// TrayFilter specifies which trays to target in a batch operation.
// If nil or empty, the operation targets all trays in the site.
type TrayFilter struct {
	RackID   *string  `json:"rackId,omitempty"`
	RackName *string  `json:"rackName,omitempty"`
	Type     *string  `json:"type,omitempty"`
	IDs      []string `json:"ids,omitempty"`
	SlotID   *int32   `json:"slotId,omitempty"` // Restrict to trays at this rack slot; requires rackId or rackName.
}

// Validate checks the tray filter fields.
func (f *TrayFilter) Validate() error {
	if f == nil {
		return nil
	}

	err := validation.ValidateStruct(f,
		validation.Field(&f.RackID, validation.When(f.RackID != nil, validation.Required)),
		validation.Field(&f.IDs, validation.Each(validation.Required)),
		validation.Field(&f.Type,
			validation.When(f.Type != nil, validation.In(validTrayTypesAny...).Error(
				fmt.Sprintf("must be one of %v", slices.Collect(maps.Keys(APIToProtoComponentTypeName)))))),
	)
	if err != nil {
		return err
	}

	if f.RackID != nil && f.RackName != nil {
		return validation.Errors{"rackId": fmt.Errorf("rackId and rackName are mutually exclusive")}
	}

	hasRackParams := f.RackID != nil || f.RackName != nil
	hasComponentParams := len(f.IDs) > 0
	if hasRackParams && hasComponentParams {
		return validation.Errors{"rackId": fmt.Errorf("rackId/rackName cannot be combined with ids")}
	}

	if err := validateSlotConstraints(f.SlotID, f.RackID, f.RackName); err != nil {
		return err
	}

	return nil
}

// HasSlotFilter reports whether the filter constrains rack slot.
// When true, callers cannot use ToTargetSpec directly: Flow has no
// by-slot component target shape, so slotId is resolved to component IDs
// via lookup first. ToTargetSpec ignores SlotID.
func (f *TrayFilter) HasSlotFilter() bool {
	return f != nil && f.SlotID != nil
}

// MatchesSlot reports whether comp satisfies the filter's slotId.
// Safe to call when no slot filter is set.
func (f *TrayFilter) MatchesSlot(comp *flowv1.Component) bool {
	if f == nil {
		return true
	}
	return RackComponentSlotMatcher{SlotID: f.SlotID}.Matches(comp)
}

func componentTargetsFromIDs(ids []string, componentType *string) []*flowv1.ComponentTarget {
	protoType := flowv1.ComponentType_COMPONENT_TYPE_UNKNOWN
	if componentType != nil {
		protoName := APIToProtoComponentTypeName[*componentType]
		protoType = flowv1.ComponentType(flowv1.ComponentType_value[protoName])
	}

	targets := make([]*flowv1.ComponentTarget, 0, len(ids))
	for _, id := range ids {
		targets = append(targets, &flowv1.ComponentTarget{
			Identifier: &flowv1.ComponentTarget_External{
				External: &flowv1.ExternalRef{Type: protoType, Id: id},
			},
		})
	}
	return targets
}

// ToTargetSpec converts the filter to an Flow OperationTargetSpec.
// Handles nil receiver gracefully (targets all trays).
func (f *TrayFilter) ToTargetSpec() *flowv1.OperationTargetSpec {
	if f == nil {
		return &flowv1.OperationTargetSpec{
			Targets: &flowv1.OperationTargetSpec_Racks{
				Racks: &flowv1.RackTargets{
					Targets: []*flowv1.RackTarget{{
						ComponentTypes: ValidProtoComponentTypes,
					}},
				},
			},
		}
	}

	if len(f.IDs) > 0 {
		return &flowv1.OperationTargetSpec{
			Targets: &flowv1.OperationTargetSpec_Components{
				Components: &flowv1.ComponentTargets{
					Targets: componentTargetsFromIDs(f.IDs, f.Type),
				},
			},
		}
	}

	rackTarget := &flowv1.RackTarget{}

	if f.RackID != nil {
		rackTarget.Identifier = &flowv1.RackTarget_ExternalId{ExternalId: *f.RackID}
	} else if f.RackName != nil {
		rackTarget.Identifier = &flowv1.RackTarget_Name{
			Name: *f.RackName,
		}
	}

	if f.Type != nil {
		if protoName, ok := APIToProtoComponentTypeName[*f.Type]; ok {
			rackTarget.ComponentTypes = []flowv1.ComponentType{
				flowv1.ComponentType(flowv1.ComponentType_value[protoName]),
			}
		}
	} else {
		rackTarget.ComponentTypes = ValidProtoComponentTypes
	}

	return &flowv1.OperationTargetSpec{
		Targets: &flowv1.OperationTargetSpec_Racks{
			Racks: &flowv1.RackTargets{
				Targets: []*flowv1.RackTarget{rackTarget},
			},
		},
	}
}

// APITrayGetAllRequest captures query parameters for listing trays from Flow.
type APITrayGetAllRequest struct {
	SiteID   string   `query:"siteId"`
	RackID   *string  `query:"rackId"`
	RackName *string  `query:"rackName"`
	Type     *string  `query:"type"`
	IDs      []string `query:"id"`
	SlotID   *int32   `query:"slotId"` // Restrict to trays at this rack slot; requires rackId or rackName.
}

// Validate checks field formats and enforces the Flow protobuf oneof constraints:
//   - rackId and rackName are mutually exclusive (RackTarget.oneof identifier)
//   - rackId/rackName cannot be combined with id (OperationTargetSpec.oneof targets)
//   - type must be one of the supported tray types
//   - slotId requires rackId or rackName and must be >= 0
func (r *APITrayGetAllRequest) Validate() error {
	err := validation.ValidateStruct(r,
		validation.Field(&r.RackID, validation.When(r.RackID != nil, validation.Required)),
		validation.Field(&r.IDs, validation.Each(validation.Required)),
		validation.Field(&r.Type,
			validation.When(r.Type != nil, validation.In(validTrayTypesAny...).Error(
				fmt.Sprintf("must be one of %v", slices.Collect(maps.Keys(APIToProtoComponentTypeName)))))),
	)
	if err != nil {
		return err
	}

	if r.RackID != nil && r.RackName != nil {
		return validation.Errors{"rackId": fmt.Errorf("rackId and rackName are mutually exclusive")}
	}

	hasRackParams := r.RackID != nil || r.RackName != nil
	hasComponentParams := len(r.IDs) > 0
	if hasRackParams && hasComponentParams {
		return validation.Errors{"rackId": fmt.Errorf("rackId/rackName cannot be combined with id")}
	}

	if err := validateSlotConstraints(r.SlotID, r.RackID, r.RackName); err != nil {
		return err
	}

	return nil
}

// HasSlotFilter reports whether the request constrains rack slot.
func (r *APITrayGetAllRequest) HasSlotFilter() bool {
	return r != nil && r.SlotID != nil
}

// MatchesSlot reports whether comp satisfies the request's slotId.
func (r *APITrayGetAllRequest) MatchesSlot(comp *flowv1.Component) bool {
	if r == nil {
		return true
	}
	return RackComponentSlotMatcher{SlotID: r.SlotID}.Matches(comp)
}

// ToProto converts a validated APITrayGetAllRequest to an Flow GetComponentsRequest.
func (r *APITrayGetAllRequest) ToProto() *flowv1.GetComponentsRequest {
	flowRequest := &flowv1.GetComponentsRequest{}

	if len(r.IDs) > 0 {
		flowRequest.TargetSpec = &flowv1.OperationTargetSpec{
			Targets: &flowv1.OperationTargetSpec_Components{
				Components: &flowv1.ComponentTargets{
					Targets: componentTargetsFromIDs(r.IDs, r.Type),
				},
			},
		}
		return flowRequest
	}

	// When a specific rack is identified, use TargetSpec with a RackTarget.
	// When no rack identifier is provided, omit TargetSpec entirely so Flow
	// queries all components, and pass Type as a filter instead.
	if r.RackID != nil || r.RackName != nil {
		rackTarget := &flowv1.RackTarget{}
		if r.RackID != nil {
			rackTarget.Identifier = &flowv1.RackTarget_ExternalId{ExternalId: *r.RackID}
		} else {
			rackTarget.Identifier = &flowv1.RackTarget_Name{
				Name: *r.RackName,
			}
		}

		if r.Type != nil {
			if protoName, ok := APIToProtoComponentTypeName[*r.Type]; ok {
				rackTarget.ComponentTypes = []flowv1.ComponentType{
					flowv1.ComponentType(flowv1.ComponentType_value[protoName]),
				}
			}
		} else {
			rackTarget.ComponentTypes = ValidProtoComponentTypes
		}

		flowRequest.TargetSpec = &flowv1.OperationTargetSpec{
			Targets: &flowv1.OperationTargetSpec_Racks{
				Racks: &flowv1.RackTargets{
					Targets: []*flowv1.RackTarget{rackTarget},
				},
			},
		}
		return flowRequest
	}

	// No rack or component targeting — query all components via filters.
	if r.Type != nil {
		if f := GetProtoTrayFilter("type", []string{*r.Type}); f != nil {
			flowRequest.Filters = append(flowRequest.Filters, f)
		}
	}

	return flowRequest
}

// QueryValues returns only the known query parameters as url.Values,
// suitable for deterministic workflow ID hashing without unknown param interference.
func (r *APITrayGetAllRequest) QueryValues() url.Values {
	v := url.Values{}
	v.Set("siteId", r.SiteID)
	if r.RackID != nil {
		v.Set("rackId", *r.RackID)
	}
	if r.RackName != nil {
		v.Set("rackName", *r.RackName)
	}
	if r.Type != nil {
		v.Set("type", *r.Type)
	}
	for _, id := range r.IDs {
		v.Add("id", id)
	}
	if r.SlotID != nil {
		v.Set("slotId", strconv.FormatInt(int64(*r.SlotID), 10))
	}
	return v
}

// APITrayValidateAllRequest captures query parameters for validating trays.
type APITrayValidateAllRequest struct {
	SiteID       string   `query:"siteId"`
	RackID       *string  `query:"rackId"`
	RackName     *string  `query:"rackName"`
	Name         []string `query:"name"`
	Manufacturer []string `query:"manufacturer"`
	Type         *string  `query:"type"`
	IDs          []string `query:"id"`
	SlotID       *int32   `query:"slotId"` // Restrict to trays at this rack slot; requires rackId or rackName.
}

// Validate checks constraints on the request parameters.
func (r *APITrayValidateAllRequest) Validate() error {
	if r.SiteID == "" {
		return fmt.Errorf("siteId query parameter is required")
	}
	if err := validation.ValidateStruct(r,
		validation.Field(&r.RackID, validation.When(r.RackID != nil, validation.Required)),
		validation.Field(&r.IDs, validation.Each(validation.Required)),
		validation.Field(&r.Type,
			validation.When(r.Type != nil, validation.In(validTrayTypesAny...).Error(
				fmt.Sprintf("must be one of %v", slices.Collect(maps.Keys(APIToProtoComponentTypeName)))))),
	); err != nil {
		return err
	}
	if r.RackID != nil && r.RackName != nil {
		return validation.Errors{"rackId": fmt.Errorf("rackId and rackName are mutually exclusive")}
	}
	hasRackScope := r.RackID != nil || r.RackName != nil
	if hasRackScope && len(r.IDs) > 0 {
		return validation.Errors{"rackId": fmt.Errorf("rackId/rackName and id are mutually exclusive")}
	}
	if err := validateSlotConstraints(r.SlotID, r.RackID, r.RackName); err != nil {
		return err
	}
	return nil
}

// HasSlotFilter reports whether the request constrains rack slot.
func (r *APITrayValidateAllRequest) HasSlotFilter() bool {
	return r != nil && r.SlotID != nil
}

// MatchesSlot reports whether comp satisfies the request's slotId.
func (r *APITrayValidateAllRequest) MatchesSlot(comp *flowv1.Component) bool {
	if r == nil {
		return true
	}
	return RackComponentSlotMatcher{SlotID: r.SlotID}.Matches(comp)
}

// ToTargetSpec converts the request's targeting fields to an Flow OperationTargetSpec.
func (r *APITrayValidateAllRequest) ToTargetSpec() *flowv1.OperationTargetSpec {
	if r.RackID != nil {
		return &flowv1.OperationTargetSpec{
			Targets: &flowv1.OperationTargetSpec_Racks{
				Racks: &flowv1.RackTargets{
					Targets: []*flowv1.RackTarget{
						{Identifier: &flowv1.RackTarget_ExternalId{ExternalId: *r.RackID}},
					},
				},
			},
		}
	}
	if r.RackName != nil {
		return &flowv1.OperationTargetSpec{
			Targets: &flowv1.OperationTargetSpec_Racks{
				Racks: &flowv1.RackTargets{
					Targets: []*flowv1.RackTarget{
						{Identifier: &flowv1.RackTarget_Name{Name: *r.RackName}},
					},
				},
			},
		}
	}
	if len(r.IDs) > 0 {
		return &flowv1.OperationTargetSpec{
			Targets: &flowv1.OperationTargetSpec_Components{
				Components: &flowv1.ComponentTargets{
					Targets: componentTargetsFromIDs(r.IDs, r.Type),
				},
			},
		}
	}
	return nil
}

// ToFilters converts the request's filter fields to Flow protobuf filters.
func (r *APITrayValidateAllRequest) ToFilters() []*flowv1.Filter {
	var filters []*flowv1.Filter
	if f := GetProtoTrayFilter("name", r.Name); f != nil {
		filters = append(filters, f)
	}
	if f := GetProtoTrayFilter("manufacturer", r.Manufacturer); f != nil {
		filters = append(filters, f)
	}
	if r.Type != nil {
		if f := GetProtoTrayFilter("type", []string{*r.Type}); f != nil {
			filters = append(filters, f)
		}
	}
	return filters
}

// QueryValues returns only the known query parameters as url.Values.
func (r *APITrayValidateAllRequest) QueryValues() url.Values {
	v := url.Values{}
	v.Set("siteId", r.SiteID)
	if r.RackID != nil {
		v.Set("rackId", *r.RackID)
	}
	if r.RackName != nil {
		v.Set("rackName", *r.RackName)
	}
	for _, n := range r.Name {
		v.Add("name", n)
	}
	for _, m := range r.Manufacturer {
		v.Add("manufacturer", m)
	}
	if r.Type != nil {
		v.Set("type", *r.Type)
	}
	for _, id := range r.IDs {
		v.Add("id", id)
	}
	if r.SlotID != nil {
		v.Set("slotId", strconv.FormatInt(int64(*r.SlotID), 10))
	}
	return v
}

// APITrayPosition represents the position of a tray within a rack
type APITrayPosition struct {
	SlotID  int32 `json:"slotId"`
	TrayIdx int32 `json:"trayIdx"`
	HostID  int32 `json:"hostId"`
}

// FromProto converts a proto RackPosition to an APITrayPosition
func (atp *APITrayPosition) FromProto(protoPosition *flowv1.RackPosition) {
	if protoPosition == nil {
		return
	}
	atp.SlotID = protoPosition.GetSlotId()
	atp.TrayIdx = protoPosition.GetTrayIdx()
	atp.HostID = protoPosition.GetHostId()
}

// APITray is the API representation of a Tray (Component) from Flow
type APITray struct {
	ID              string           `json:"id"`
	Type            string           `json:"type"`
	Name            string           `json:"name"`
	Manufacturer    string           `json:"manufacturer"`
	Model           string           `json:"model"`
	SerialNumber    string           `json:"serialNumber"`
	Description     string           `json:"description"`
	FirmwareVersion string           `json:"firmwareVersion"`
	PowerState      string           `json:"powerState"`
	OperationStatus string           `json:"operationStatus"`
	LeakStatus      string           `json:"leakStatus"`
	Position        *APITrayPosition `json:"position"`
	BMCs            []*APIBMC        `json:"bmcs"`
	RackID          string           `json:"rackId"`
	NVLinkDomainID  *string          `json:"nvLinkDomainId"`
	TaskStats       APITaskStats     `json:"taskStats"`
}

// FromProto converts an Flow protobuf Component to an APITray
func (at *APITray) FromProto(comp *flowv1.Component) {
	if comp == nil {
		return
	}

	at.Type = enumOr(ProtoToAPIComponentTypeName, comp.GetType(), "unknown")
	at.FirmwareVersion = comp.GetFirmwareVersion()
	at.PowerState = comp.GetPowerState()
	at.OperationStatus = enumOr(ProtoToAPIPhaseName, comp.GetStatus().GetPhase(), "Unknown")
	at.LeakStatus = enumOr(ProtoToAPILeakStatusName, comp.GetLeakStatus(), "Unknown")
	at.ID = comp.GetComponentId()
	at.TaskStats.FromProto(comp.GetTaskStats())

	// Get info from DeviceInfo
	if comp.GetInfo() != nil {
		info := comp.GetInfo()
		at.Name = info.GetName()
		at.Manufacturer = info.GetManufacturer()
		if info.Model != nil {
			at.Model = *info.Model
		}
		at.SerialNumber = info.GetSerialNumber()
		if info.Description != nil {
			at.Description = *info.Description
		}
	}

	// Get position
	if comp.GetPosition() != nil {
		at.Position = &APITrayPosition{}
		at.Position.FromProto(comp.GetPosition())
	}

	// Get BMCs
	if len(comp.GetBmcs()) > 0 {
		at.BMCs = make([]*APIBMC, 0, len(comp.GetBmcs()))
		for _, bmc := range comp.GetBmcs() {
			apiBMC := &APIBMC{}
			apiBMC.FromProto(bmc)
			at.BMCs = append(at.BMCs, apiBMC)
		}
	}

	// Get rack ID
	at.RackID = comp.GetRackExternalId()

	if comp.GetNvlDomainId() != nil {
		domainID := comp.GetNvlDomainId().GetId()
		at.NVLinkDomainID = &domainID
	}
}

// NewAPITray creates an APITray from the Flow protobuf Component
func NewAPITray(comp *flowv1.Component) *APITray {
	if comp == nil {
		return nil
	}
	apiTray := &APITray{}
	apiTray.FromProto(comp)
	return apiTray
}
