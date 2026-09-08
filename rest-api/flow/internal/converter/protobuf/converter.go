// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package protobuf

import (
	"encoding/json"
	"errors"
	"fmt"
	"net"
	"time"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"
	"google.golang.org/protobuf/types/known/timestamppb"

	dbquery "github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/query"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operationrules"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	taskdef "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	identifier "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/Identifier"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/deviceinfo"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/location"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/bmc"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/component"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/nvldomain"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/rack"
	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
)

var (
	componentTypeToMap   map[devicetypes.ComponentType]pb.ComponentType
	bmcTypeToMap         map[devicetypes.BMCType]pb.BMCType
	componentTypeFromMap map[pb.ComponentType]devicetypes.ComponentType
	bmcTypeFromMap       map[pb.BMCType]devicetypes.BMCType
)

func init() {
	// Initialize the mappings between internal component types and protobuf
	// component types
	componentTypeToMap = map[devicetypes.ComponentType]pb.ComponentType{
		devicetypes.ComponentTypeUnknown:    pb.ComponentType_COMPONENT_TYPE_UNKNOWN,
		devicetypes.ComponentTypeCompute:    pb.ComponentType_COMPONENT_TYPE_COMPUTE,
		devicetypes.ComponentTypeNVSwitch:   pb.ComponentType_COMPONENT_TYPE_NVSWITCH,
		devicetypes.ComponentTypePowerShelf: pb.ComponentType_COMPONENT_TYPE_POWERSHELF,
		devicetypes.ComponentTypeToRSwitch:  pb.ComponentType_COMPONENT_TYPE_TORSWITCH,
		devicetypes.ComponentTypeUMS:        pb.ComponentType_COMPONENT_TYPE_UMS,
		devicetypes.ComponentTypeCDU:        pb.ComponentType_COMPONENT_TYPE_CDU,
	}

	// Initialize the mappings between internal BMC types and protobuf BMC
	// types
	bmcTypeToMap = map[devicetypes.BMCType]pb.BMCType{
		devicetypes.BMCTypeUnknown: pb.BMCType_BMC_TYPE_UNKNOWN,
		devicetypes.BMCTypeHost:    pb.BMCType_BMC_TYPE_HOST,
		devicetypes.BMCTypeDPU:     pb.BMCType_BMC_TYPE_DPU,
	}

	// Reverse mappings for component types
	componentTypeFromMap = make(map[pb.ComponentType]devicetypes.ComponentType)
	for t, pt := range componentTypeToMap {
		componentTypeFromMap[pt] = t
	}

	// Reverse mappings for BMC types
	bmcTypeFromMap = make(map[pb.BMCType]devicetypes.BMCType)
	for t, pt := range bmcTypeToMap {
		bmcTypeFromMap[pt] = t
	}
}

// ComponentTypeFrom converts a protobuf ComponentType to an internal
// ComponentType
func ComponentTypeFrom(pt pb.ComponentType) devicetypes.ComponentType {
	if t, ok := componentTypeFromMap[pt]; ok {
		return t
	}

	return devicetypes.ComponentTypeUnknown
}

// BMCTypeFrom converts a protobuf BMCType to an internal BMCType
func BMCTypeFrom(pt pb.BMCType) devicetypes.BMCType {
	if t, ok := bmcTypeFromMap[pt]; ok {
		return t
	}

	return devicetypes.BMCTypeUnknown
}

// DeviceInfoFrom converts a protobuf DeviceInfo to an internal DeviceInfo
func DeviceInfoFrom(info *pb.DeviceInfo) deviceinfo.DeviceInfo {
	if info == nil {
		return deviceinfo.DeviceInfo{}
	}

	// No need to check whether the info is nil. They are handled by the
	// methods of pb.DeviceInfo.
	return deviceinfo.DeviceInfo{
		ID:           UUIDFrom(info.GetId()),
		Name:         info.GetName(),
		Manufacturer: info.GetManufacturer(),
		Model:        info.GetModel(),
		SerialNumber: info.GetSerialNumber(),
		Description:  info.GetDescription(),
	}
}

// LocationFrom converts a protobuf Location to an internal Location
func LocationFrom(loc *pb.Location) location.Location {
	if loc == nil {
		return location.Location{}
	}

	return location.Location{
		Region:     loc.GetRegion(),
		DataCenter: loc.GetDatacenter(),
		Room:       loc.GetRoom(),
		Position:   loc.GetPosition(),
	}
}

// UUIDFrom converts a protobuf UUID to an internal uuid.UUID
func UUIDFrom(id *pb.UUID) uuid.UUID {
	if id != nil {
		if parsed, err := uuid.Parse(id.Id); err == nil {
			return parsed
		}
	}

	return uuid.Nil
}

// UUIDStringFrom converts a *pb.UUID to a plain string.
// Returns "" if the input is nil or cannot be parsed.
func UUIDStringFrom(id *pb.UUID) string {
	parsed := UUIDFrom(id)
	if parsed == uuid.Nil {
		return ""
	}
	return parsed.String()
}

// OptionalUUIDFrom converts a *pb.UUID to *uuid.UUID.
// Returns nil if the input is nil or cannot be parsed.
func OptionalUUIDFrom(id *pb.UUID) *uuid.UUID {
	parsed := UUIDFrom(id)
	if parsed == uuid.Nil {
		return nil
	}
	return &parsed
}

// UUIDsFrom converts a slice of *pb.UUID to a slice of uuid.UUID.
func UUIDsFrom(ids []*pb.UUID) []uuid.UUID {
	result := make([]uuid.UUID, 0, len(ids))
	for _, id := range ids {
		if parsed := UUIDFrom(id); parsed != uuid.Nil {
			result = append(result, parsed)
		}
	}
	return result
}

// RackPositionFrom converts a protobuf RackPosition to an internal
// InRackPosition
func RackPositionFrom(pos *pb.RackPosition) component.InRackPosition {
	if pos == nil {
		// Return invalid position if the postion is not specified.
		return component.InRackPosition{
			SlotID:    -1,
			TrayIndex: -1,
			HostID:    -1,
		}
	}

	return component.InRackPosition{
		SlotID:    int(pos.SlotId),
		TrayIndex: int(pos.TrayIdx),
		HostID:    int(pos.HostId),
	}
}

// BMCFrom converts a protobuf BMCInfo to an internal BMCType and BMC
func BMCFrom(bi *pb.BMCInfo) (devicetypes.BMCType, *bmc.BMC) {
	if bi == nil {
		return devicetypes.BMCTypeUnknown, nil
	}

	var b bmc.BMC

	if hwAddr, err := net.ParseMAC(bi.GetMacAddress()); err == nil {
		b.MAC = bmc.MACAddress{HardwareAddr: hwAddr}
	}

	if ip := bi.GetIpAddress(); ip != "" {
		b.IP = net.ParseIP(ip)
	}

	return BMCTypeFrom(bi.Type), &b
}

// BMCsFrom converts a slice of protobuf BMCInfo to the internal BMC map keyed by type.
func BMCsFrom(pbBmcs []*pb.BMCInfo) map[devicetypes.BMCType][]bmc.BMC {
	bmcsByType := make(map[devicetypes.BMCType][]bmc.BMC)
	for _, bi := range pbBmcs {
		t, b := BMCFrom(bi)
		if b != nil {
			bmcsByType[t] = append(bmcsByType[t], *b)
		}
	}
	return bmcsByType
}

// ComponentFrom converts a protobuf Component to an internal Component
func ComponentFrom(c *pb.Component) *component.Component {
	if c == nil {
		return nil
	}

	bmcsByType := BMCsFrom(c.GetBmcs())

	return &component.Component{
		Type:            ComponentTypeFrom(c.GetType()),
		Info:            DeviceInfoFrom(c.GetInfo()),
		FirmwareVersion: c.GetFirmwareVersion(),
		Position:        RackPositionFrom(c.GetPosition()),
		BmcsByType:      bmcsByType,
		NVLDomainID:     UUIDFrom(c.GetNvlDomainId()),
		PowerState:      c.GetPowerState(),
		RackExternalID:  c.GetRackExternalId(),
	}
}

// RackFrom converts a protobuf Rack to an internal Rack
func RackFrom(r *pb.Rack) *rack.Rack {
	if r == nil {
		return nil
	}

	domainIDs := UUIDsFrom(r.GetNvlDomainIds())
	var domainID uuid.UUID
	if len(domainIDs) > 0 {
		domainID = domainIDs[0]
	}
	if len(domainIDs) > 1 {
		log.Warn().
			Int("domain_id_count", len(domainIDs)).
			Str("rack_id", UUIDFrom(r.GetInfo().GetId()).String()).
			Msg("Rack has multiple NVLink domain IDs; using the first")
	}

	components := make([]component.Component, 0, len(r.GetComponents()))
	for _, c := range r.GetComponents() {
		converted := ComponentFrom(c)
		if converted.NVLDomainID == uuid.Nil {
			converted.NVLDomainID = domainID
		}
		components = append(components, *converted)
	}
	result := &rack.Rack{
		Info:       DeviceInfoFrom(r.GetInfo()),
		ExternalID: r.GetExternalId(),
		Loc:        LocationFrom(r.GetLocation()),
		Components: components,
	}
	result.NVLDomainID = domainID
	return result
}

// PaginationFrom converts a protobuf Pagination to an internal Pagination.
func PaginationFrom(pg *pb.Pagination) *dbquery.Pagination {
	if pg == nil {
		return dbquery.DefaultPagination()
	}

	return &dbquery.Pagination{
		Offset: int(pg.GetOffset()),
		Limit:  int(pg.GetLimit()),
	}
}

// StringQueryInfoFrom converts a protobuf StringQueryInfo to an internal StringQueryInfo.
func StringQueryInfoFrom(info *pb.StringQueryInfo) *dbquery.StringQueryInfo {
	if info == nil {
		return nil
	}

	return &dbquery.StringQueryInfo{
		Patterns:   info.GetPatterns(),
		IsWildcard: info.GetIsWildcard(),
		UseOR:      info.GetUseOr(),
	}
}

// FilterFrom converts a protobuf Filter to internal filter fields.
// Returns: (fieldName, StringQueryInfo, error)
// The filter type (rack vs component) is inferred from which field is set
// in the Filter message (rack_field or component_field).
func FilterFrom(filter *pb.Filter) (string, *dbquery.StringQueryInfo, error) {
	if filter == nil || filter.GetQueryInfo() == nil {
		return "", nil, nil
	}

	var fieldName string
	switch {
	case filter.GetRackField() != pb.RackFilterField_RACK_FILTER_FIELD_UNSPECIFIED:
		fieldName = rackFilterFieldToColumn(filter.GetRackField())
		if fieldName == "" {
			return "", nil, fmt.Errorf("unsupported rack filter field: %v",
				filter.GetRackField())
		}
	case filter.GetComponentField() != pb.ComponentFilterField_COMPONENT_FILTER_FIELD_UNSPECIFIED:
		fieldName = componentFilterFieldToColumn(filter.GetComponentField())
		if fieldName == "" {
			return "", nil, fmt.Errorf("unsupported component filter field: %v",
				filter.GetComponentField())
		}
	default:
		return "", nil, fmt.Errorf("filter field not set: either rack_field or " +
			"component_field must be set")
	}

	queryInfo := StringQueryInfoFrom(filter.GetQueryInfo())
	return fieldName, queryInfo, nil
}

// rackFilterFieldToColumn converts RackFilterField enum to column name
func rackFilterFieldToColumn(field pb.RackFilterField) string {
	switch field {
	case pb.RackFilterField_RACK_FILTER_FIELD_NAME:
		return "name"
	case pb.RackFilterField_RACK_FILTER_FIELD_MANUFACTURER:
		return "manufacturer"
	case pb.RackFilterField_RACK_FILTER_FIELD_MODEL:
		return "description->>'model'"
	default:
		return ""
	}
}

// componentFilterFieldToColumn converts ComponentFilterField enum to column name
func componentFilterFieldToColumn(field pb.ComponentFilterField) string {
	switch field {
	case pb.ComponentFilterField_COMPONENT_FILTER_FIELD_NAME:
		return "name"
	case pb.ComponentFilterField_COMPONENT_FILTER_FIELD_MANUFACTURER:
		return "manufacturer"
	case pb.ComponentFilterField_COMPONENT_FILTER_FIELD_MODEL:
		return "model"
	case pb.ComponentFilterField_COMPONENT_FILTER_FIELD_TYPE:
		return "type"
	default:
		return ""
	}
}

// IdentifierFrom converts a protobuf Identifier to an internal Identifier.
func IdentifierFrom(info *pb.Identifier) *identifier.Identifier {
	if info == nil {
		return nil
	}

	return identifier.New(UUIDFrom(info.GetId()), info.GetName())
}

// NVLDomainFrom converts a protobuf NVLDomain to an internal NVLDomain.
func NVLDomainFrom(info *pb.NVLDomain) *nvldomain.NVLDomain {
	if info == nil || info.GetIdentifier() == nil {
		return nil
	}

	return &nvldomain.NVLDomain{
		Identifier: *IdentifierFrom(info.GetIdentifier()),
	}
}

// PowerControlOpFrom converts a protobuf PowerControlOp to an internal PowerOperation.
func PowerControlOpFrom(op pb.PowerControlOp) operations.PowerOperation {
	switch op {
	// Power On
	case pb.PowerControlOp_POWER_CONTROL_OP_ON:
		return operations.PowerOperationPowerOn
	case pb.PowerControlOp_POWER_CONTROL_OP_FORCE_ON:
		return operations.PowerOperationForcePowerOn
	// Power Off
	case pb.PowerControlOp_POWER_CONTROL_OP_OFF:
		return operations.PowerOperationPowerOff
	case pb.PowerControlOp_POWER_CONTROL_OP_FORCE_OFF:
		return operations.PowerOperationForcePowerOff
	// Restart (OS level)
	case pb.PowerControlOp_POWER_CONTROL_OP_RESTART:
		return operations.PowerOperationRestart
	case pb.PowerControlOp_POWER_CONTROL_OP_FORCE_RESTART:
		return operations.PowerOperationForceRestart
	// Reset (hardware level)
	case pb.PowerControlOp_POWER_CONTROL_OP_WARM_RESET:
		return operations.PowerOperationWarmReset
	case pb.PowerControlOp_POWER_CONTROL_OP_COLD_RESET:
		return operations.PowerOperationColdReset
	default:
		return operations.PowerOperationUnknown
	}
}

// TaskExecutorTypeFrom converts a protobuf TaskExecutorType to an internal ExecutorType.
func TaskExecutorTypeFrom(et pb.TaskExecutorType) taskcommon.ExecutorType {
	switch et {
	case pb.TaskExecutorType_TASK_EXECUTOR_TYPE_TEMPORAL:
		return taskcommon.ExecutorTypeTemporal
	default:
		return taskcommon.ExecutorTypeUnknown
	}
}

// TaskExecutorTypeTo converts an internal ExecutorType to a protobuf TaskExecutorType.
func TaskExecutorTypeTo(et taskcommon.ExecutorType) pb.TaskExecutorType {
	switch et {
	case taskcommon.ExecutorTypeTemporal:
		return pb.TaskExecutorType_TASK_EXECUTOR_TYPE_TEMPORAL
	default:
		return pb.TaskExecutorType_TASK_EXECUTOR_TYPE_UNKNOWN
	}
}

// TaskStatusFrom converts a protobuf TaskStatus to an internal TaskStatus.
func TaskStatusFrom(status pb.TaskStatus) taskcommon.TaskStatus {
	switch status {
	case pb.TaskStatus_TASK_STATUS_PENDING:
		return taskcommon.TaskStatusPending
	case pb.TaskStatus_TASK_STATUS_RUNNING:
		return taskcommon.TaskStatusRunning
	case pb.TaskStatus_TASK_STATUS_COMPLETED:
		return taskcommon.TaskStatusCompleted
	case pb.TaskStatus_TASK_STATUS_FAILED:
		return taskcommon.TaskStatusFailed
	case pb.TaskStatus_TASK_STATUS_TERMINATED:
		return taskcommon.TaskStatusTerminated
	case pb.TaskStatus_TASK_STATUS_WAITING:
		return taskcommon.TaskStatusWaiting
	default:
		return taskcommon.TaskStatusUnknown
	}
}

// TaskStatusTo converts an internal TaskStatus to a protobuf TaskStatus.
func TaskStatusTo(status taskcommon.TaskStatus) pb.TaskStatus {
	switch status {
	case taskcommon.TaskStatusPending:
		return pb.TaskStatus_TASK_STATUS_PENDING
	case taskcommon.TaskStatusRunning:
		return pb.TaskStatus_TASK_STATUS_RUNNING
	case taskcommon.TaskStatusCompleted:
		return pb.TaskStatus_TASK_STATUS_COMPLETED
	case taskcommon.TaskStatusFailed:
		return pb.TaskStatus_TASK_STATUS_FAILED
	case taskcommon.TaskStatusTerminated:
		return pb.TaskStatus_TASK_STATUS_TERMINATED
	case taskcommon.TaskStatusWaiting:
		return pb.TaskStatus_TASK_STATUS_WAITING
	default:
		return pb.TaskStatus_TASK_STATUS_UNKNOWN
	}
}

func TaskTo(task *taskdef.Task) *pb.Task {
	if task == nil {
		return nil
	}

	var opStr string
	operation, err := operations.New(task.Operation.Type, task.Operation.Info)
	if err == nil {
		opStr = operation.Description()
	}

	pbTask := &pb.Task{
		Id:             UUIDTo(task.ID),
		Operation:      opStr,
		RackId:         UUIDTo(task.RackID),
		ComponentUuids: UUIDsTo(task.Attributes.AllComponentUUIDs()),
		Description:    task.Description,
		ExecutorType:   TaskExecutorTypeTo(task.ExecutorType),
		ExecutionId:    task.ExecutionID,
		Status:         TaskStatusTo(task.Status),
		Message:        task.Message,
		Report:         string(task.Report),
		CreatedAt:      timestamppb.New(task.CreatedAt),
		UpdatedAt:      timestamppb.New(task.UpdatedAt),
	}
	if task.AppliedRuleID != nil {
		pbTask.AppliedRuleId = UUIDTo(*task.AppliedRuleID)
	}
	if task.StartedAt != nil {
		pbTask.StartedAt = timestamppb.New(*task.StartedAt)
	}
	if task.FinishedAt != nil {
		pbTask.FinishedAt = timestamppb.New(*task.FinishedAt)
	}

	if task.QueueExpiresAt != nil {
		pbTask.QueueExpiresAt = timestamppb.New(*task.QueueExpiresAt)
	}

	return pbTask
}

// ComponentTypeTo converts an internal ComponentType to a protobuf
// ComponentType
func ComponentTypeTo(t devicetypes.ComponentType) pb.ComponentType {
	if pt, ok := componentTypeToMap[t]; ok {
		return pt
	}

	return pb.ComponentType_COMPONENT_TYPE_UNKNOWN
}

// BMCTypeTo converts an internal BMCType to a protobuf BMCType
func BMCTypeTo(t devicetypes.BMCType) pb.BMCType {
	if pt, ok := bmcTypeToMap[t]; ok {
		return pt
	}

	return pb.BMCType_BMC_TYPE_UNKNOWN
}

// DeviceInfoTo converts an internal DeviceInfo to a protobuf DeviceInfo
func DeviceInfoTo(info *deviceinfo.DeviceInfo) *pb.DeviceInfo {
	if info == nil {
		return nil
	}

	pinfo := pb.DeviceInfo{
		Id:           UUIDTo(info.ID),
		Name:         info.Name,
		Manufacturer: info.Manufacturer,
		SerialNumber: info.SerialNumber,
	}

	if model := info.Model; model != "" {
		pinfo.Model = &model
	}

	if description := info.Description; description != "" {
		pinfo.Description = &description
	}

	return &pinfo
}

// LocationTo converts an internal Location to a protobuf Location
func LocationTo(loc *location.Location) *pb.Location {
	if loc == nil {
		return nil
	}

	return &pb.Location{
		Region:     loc.Region,
		Datacenter: loc.DataCenter,
		Room:       loc.Room,
		Position:   loc.Position,
	}
}

// UUIDTo converts an internal uuid.UUID to a protobuf UUID
func UUIDTo(id uuid.UUID) *pb.UUID {
	if id != uuid.Nil {
		return &pb.UUID{Id: id.String()}
	}

	return nil
}

// UUIDsTo converts a slice of uuid.UUID to a slice of *pb.UUID.
func UUIDsTo(ids []uuid.UUID) []*pb.UUID {
	result := make([]*pb.UUID, 0, len(ids))
	for _, id := range ids {
		if id != uuid.Nil {
			result = append(result, &pb.UUID{Id: id.String()})
		}
	}
	return result
}

// RackPositionTo converts an internal InRackPosition to a protobuf
// RackPosition
func RackPositionTo(pos *component.InRackPosition) *pb.RackPosition {
	if pos == nil {
		return nil
	}

	return &pb.RackPosition{
		SlotId:  int32(pos.SlotID),
		TrayIdx: int32(pos.TrayIndex),
		HostId:  int32(pos.HostID),
	}
}

// BMCTo converts an internal BMCType and BMC to a protobuf BMCInfo
func BMCTo(t devicetypes.BMCType, b *bmc.BMC) *pb.BMCInfo {
	if b == nil {
		return nil
	}

	bmcProto := pb.BMCInfo{
		Type:       BMCTypeTo(t),
		MacAddress: b.MAC.String(),
	}

	if b.IP != nil {
		ip := b.IP.String()
		bmcProto.IpAddress = &ip
	}

	return &bmcProto
}

// ComponentTo converts an internal Component to a protobuf Component
func ComponentTo(c *component.Component) *pb.Component {
	if c == nil {
		return nil
	}

	bmcInfos := make([]*pb.BMCInfo, 0)
	for t, bmcs := range c.BmcsByType {
		for _, bmc := range bmcs {
			bmcInfos = append(bmcInfos, BMCTo(t, &bmc))
		}
	}

	return &pb.Component{
		Type:            ComponentTypeTo(c.Type),
		Info:            DeviceInfoTo(&c.Info),
		FirmwareVersion: c.FirmwareVersion,
		Position:        RackPositionTo(&c.Position),
		Bmcs:            bmcInfos,
		ComponentId:     c.ComponentID,
		RackId:          UUIDTo(c.RackID),
		NvlDomainId:     UUIDTo(c.NVLDomainID),
		PowerState:      c.PowerState,
		Status:          ComponentOperationStatusTo(c.Status),
		LeakStatus:      LeakStatusTo(c.LeakStatus),
		RackExternalId:  c.RackExternalID,
	}
}

// LeakStatusTo converts the Flow-internal LeakStatus to its protobuf
// counterpart. An unset or unrecognized value maps to LEAK_STATUS_UNKNOWN.
func LeakStatusTo(s types.LeakStatus) pb.LeakStatus {
	switch s {
	case types.LeakStatusDetected:
		return pb.LeakStatus_LEAK_STATUS_DETECTED
	case types.LeakStatusNotDetected:
		return pb.LeakStatus_LEAK_STATUS_NOT_DETECTED
	default:
		return pb.LeakStatus_LEAK_STATUS_UNKNOWN
	}
}

// PhaseTo converts an internal Phase to a protobuf Phase.
func PhaseTo(p types.Phase) pb.Phase {
	switch p {
	case types.PhaseInitializing:
		return pb.Phase_PHASE_INITIALIZING
	case types.PhaseReady:
		return pb.Phase_PHASE_READY
	case types.PhaseInUse:
		return pb.Phase_PHASE_IN_USE
	case types.PhaseError:
		return pb.Phase_PHASE_ERROR
	case types.PhaseDeleting:
		return pb.Phase_PHASE_DELETING
	default:
		return pb.Phase_PHASE_UNKNOWN
	}
}

// operationTypeFromTypesTo converts a Flow types.OperationType into its
// protobuf counterpart. Distinct from OperationTypeToProto (which converts
// from taskcommon.TaskType).
func operationTypeFromTypesTo(op types.OperationType) pb.OperationType {
	switch op {
	case types.OperationTypePowerControl:
		return pb.OperationType_OPERATION_TYPE_POWER_CONTROL
	case types.OperationTypeFirmwareControl:
		return pb.OperationType_OPERATION_TYPE_FIRMWARE_CONTROL
	default:
		return pb.OperationType_OPERATION_TYPE_UNKNOWN
	}
}

// ComponentOperationStatusTo converts the Flow-internal ComponentOperationStatus to the
// protobuf form. Returns nil if the input is nil so callers transparently
// surface "no status yet" rather than a default-valued message.
func ComponentOperationStatusTo(s *types.ComponentOperationStatus) *pb.ComponentOperationStatus {
	if s == nil {
		return nil
	}
	var blocked []pb.OperationType
	if len(s.BlockedOperations) > 0 {
		blocked = make([]pb.OperationType, 0, len(s.BlockedOperations))
		for _, op := range s.BlockedOperations {
			blocked = append(blocked, operationTypeFromTypesTo(op))
		}
	}
	return &pb.ComponentOperationStatus{
		Phase:             PhaseTo(s.Phase),
		Reason:            s.Reason,
		BlockedOperations: blocked,
	}
}

// RackTo converts an internal Rack to a protobuf Rack
func RackTo(r *rack.Rack) *pb.Rack {
	if r == nil {
		return nil
	}

	components := make([]*pb.Component, 0, len(r.Components))
	for _, c := range r.Components {
		if c.NVLDomainID == uuid.Nil {
			c.NVLDomainID = r.NVLDomainID
		}
		components = append(components, ComponentTo(&c))
	}

	result := &pb.Rack{
		Info:       DeviceInfoTo(&r.Info),
		ExternalId: r.ExternalID,
		Location:   LocationTo(&r.Loc),
		Components: components,
	}
	if r.NVLDomainID != uuid.Nil {
		result.NvlDomainIds = UUIDsTo([]uuid.UUID{r.NVLDomainID})
	}

	return result
}

// PaginationTo converts an internal Pagination to a protobuf Pagination.
func PaginationTo(pg *dbquery.Pagination) *pb.Pagination {
	if pg == nil {
		return nil
	}

	return &pb.Pagination{
		Offset: int32(pg.Offset),
		Limit:  int32(pg.Limit),
	}
}

// StringQueryInfoTo converts an internal StringQueryInfo to a protobuf StringQueryInfo.
func StringQueryInfoTo(info *dbquery.StringQueryInfo) *pb.StringQueryInfo {
	if info == nil {
		return nil
	}

	return &pb.StringQueryInfo{
		Patterns:   info.Patterns,
		IsWildcard: info.IsWildcard,
		UseOr:      info.UseOR,
	}
}

// OrderByFrom converts a protobuf OrderBy to an internal OrderBy
func OrderByFrom(ob *pb.OrderBy) *dbquery.OrderBy {
	if ob == nil {
		return nil
	}

	var column string
	rackField := ob.GetRackField()
	componentField := ob.GetComponentField()

	if rackField != pb.RackOrderByField_RACK_ORDER_BY_FIELD_UNSPECIFIED {
		column = rackOrderByFieldToColumn(rackField)
	} else if componentField != pb.ComponentOrderByField_COMPONENT_ORDER_BY_FIELD_UNSPECIFIED {
		column = componentOrderByFieldToColumn(componentField)
	} else {
		return nil
	}

	if column == "" {
		return nil
	}

	return &dbquery.OrderBy{
		Column:    column,
		Direction: dbquery.OrderDirection(ob.GetDirection()),
	}
}

// QueryType represents the type of query (rack or component)
type QueryType int

const (
	QueryTypeRack      QueryType = iota // QueryTypeRack identifies a rack-scoped query.
	QueryTypeComponent                  // QueryTypeComponent identifies a component-scoped query.
)

// OrderByTo converts an internal OrderBy to a protobuf OrderBy
func OrderByTo(ob *dbquery.OrderBy, queryType QueryType) *pb.OrderBy {
	if ob == nil {
		return nil
	}

	var pbOrderBy *pb.OrderBy
	switch queryType {
	case QueryTypeRack:
		field := rackOrderByColumnToField(ob.Column)
		if field == pb.RackOrderByField_RACK_ORDER_BY_FIELD_UNSPECIFIED {
			return nil
		}
		pbOrderBy = &pb.OrderBy{
			Field:     &pb.OrderBy_RackField{RackField: field},
			Direction: string(ob.Direction),
		}
	case QueryTypeComponent:
		field := componentOrderByColumnToField(ob.Column)
		if field == pb.ComponentOrderByField_COMPONENT_ORDER_BY_FIELD_UNSPECIFIED {
			return nil
		}
		pbOrderBy = &pb.OrderBy{
			Field:     &pb.OrderBy_ComponentField{ComponentField: field},
			Direction: string(ob.Direction),
		}
	default:
		return nil
	}

	return pbOrderBy
}

// rackOrderByFieldToColumn converts RackOrderByField enum to column name
func rackOrderByFieldToColumn(field pb.RackOrderByField) string {
	switch field {
	case pb.RackOrderByField_RACK_ORDER_BY_FIELD_NAME:
		return "name"
	case pb.RackOrderByField_RACK_ORDER_BY_FIELD_MANUFACTURER:
		return "manufacturer"
	case pb.RackOrderByField_RACK_ORDER_BY_FIELD_MODEL:
		return "description->>'model'"
	default:
		return ""
	}
}

// componentOrderByFieldToColumn converts ComponentOrderByField enum to column name
func componentOrderByFieldToColumn(field pb.ComponentOrderByField) string {
	switch field {
	case pb.ComponentOrderByField_COMPONENT_ORDER_BY_FIELD_NAME:
		return "name"
	case pb.ComponentOrderByField_COMPONENT_ORDER_BY_FIELD_MANUFACTURER:
		return "manufacturer"
	case pb.ComponentOrderByField_COMPONENT_ORDER_BY_FIELD_MODEL:
		return "model"
	case pb.ComponentOrderByField_COMPONENT_ORDER_BY_FIELD_TYPE:
		return "type"
	default:
		return ""
	}
}

// rackOrderByColumnToField converts column name to RackOrderByField enum
func rackOrderByColumnToField(column string) pb.RackOrderByField {
	switch column {
	case "name":
		return pb.RackOrderByField_RACK_ORDER_BY_FIELD_NAME
	case "manufacturer":
		return pb.RackOrderByField_RACK_ORDER_BY_FIELD_MANUFACTURER
	case "description->>'model'":
		return pb.RackOrderByField_RACK_ORDER_BY_FIELD_MODEL
	default:
		return pb.RackOrderByField_RACK_ORDER_BY_FIELD_UNSPECIFIED
	}
}

// componentOrderByColumnToField converts column name to ComponentOrderByField enum
func componentOrderByColumnToField(column string) pb.ComponentOrderByField {
	switch column {
	case "name":
		return pb.ComponentOrderByField_COMPONENT_ORDER_BY_FIELD_NAME
	case "manufacturer":
		return pb.ComponentOrderByField_COMPONENT_ORDER_BY_FIELD_MANUFACTURER
	case "model":
		return pb.ComponentOrderByField_COMPONENT_ORDER_BY_FIELD_MODEL
	case "type":
		return pb.ComponentOrderByField_COMPONENT_ORDER_BY_FIELD_TYPE
	default:
		return pb.ComponentOrderByField_COMPONENT_ORDER_BY_FIELD_UNSPECIFIED
	}
}

func IdentifierTo(info *identifier.Identifier) *pb.Identifier {
	if info == nil {
		return nil
	}

	return &pb.Identifier{
		Id:   UUIDTo(info.ID),
		Name: info.Name,
	}
}

func NVLDomainTo(info *nvldomain.NVLDomain) *pb.NVLDomain {
	if info == nil {
		return nil
	}

	return &pb.NVLDomain{
		Identifier: IdentifierTo(&info.Identifier),
	}
}

// ========================================
// Operation Rule Converters
// ========================================

// OperationTypeToProto converts an internal TaskType to a protobuf OperationType.
func OperationTypeToProto(opType taskcommon.TaskType) pb.OperationType {
	switch opType {
	case taskcommon.TaskTypePowerControl:
		return pb.OperationType_OPERATION_TYPE_POWER_CONTROL
	case taskcommon.TaskTypeFirmwareControl:
		return pb.OperationType_OPERATION_TYPE_FIRMWARE_CONTROL
	default:
		return pb.OperationType_OPERATION_TYPE_UNKNOWN
	}
}

// OperationTypeFromProto converts a protobuf OperationType to an internal TaskType.
func OperationTypeFromProto(opType pb.OperationType) taskcommon.TaskType {
	switch opType {
	case pb.OperationType_OPERATION_TYPE_POWER_CONTROL:
		return taskcommon.TaskTypePowerControl
	case pb.OperationType_OPERATION_TYPE_FIRMWARE_CONTROL:
		return taskcommon.TaskTypeFirmwareControl
	default:
		return taskcommon.TaskTypeUnknown
	}
}

// OperationRuleTo converts an internal OperationRule to its protobuf representation.
func OperationRuleTo(rule *operationrules.OperationRule) (*pb.OperationRule, error) {
	if rule == nil {
		return nil, nil
	}

	// Marshal RuleDefinition to JSON string
	ruleDefJSON, err := json.Marshal(rule.RuleDefinition)
	if err != nil {
		return nil, err
	}

	return &pb.OperationRule{
		Id:                 UUIDTo(rule.ID),
		Name:               rule.Name,
		Description:        rule.Description,
		OperationType:      OperationTypeToProto(rule.OperationType),
		OperationCode:      rule.OperationCode,
		RuleDefinitionJson: string(ruleDefJSON),
		IsDefault:          rule.IsDefault,
		CreatedAt:          timestamppb.New(rule.CreatedAt),
		UpdatedAt:          timestamppb.New(rule.UpdatedAt),
	}, nil
}

// OperationRuleFromProto converts a protobuf OperationRule to its internal representation.
func OperationRuleFromProto(pbRule *pb.OperationRule) (*operationrules.OperationRule, error) {
	if pbRule == nil {
		return nil, nil
	}

	// Unmarshal RuleDefinition from JSON string
	var ruleDef operationrules.RuleDefinition
	if err := json.Unmarshal([]byte(pbRule.GetRuleDefinitionJson()), &ruleDef); err != nil {
		return nil, err
	}

	rule := &operationrules.OperationRule{
		ID:             UUIDFrom(pbRule.GetId()),
		Name:           pbRule.GetName(),
		Description:    pbRule.GetDescription(),
		OperationType:  OperationTypeFromProto(pbRule.GetOperationType()),
		OperationCode:  pbRule.GetOperationCode(),
		RuleDefinition: ruleDef,
		IsDefault:      pbRule.GetIsDefault(),
	}

	if pbRule.GetCreatedAt() != nil {
		rule.CreatedAt = pbRule.GetCreatedAt().AsTime()
	}
	if pbRule.GetUpdatedAt() != nil {
		rule.UpdatedAt = pbRule.GetUpdatedAt().AsTime()
	}

	return rule, nil
}

// RackRuleAssociationTo converts domain object to protobuf
func RackRuleAssociationTo(assoc *operationrules.RackRuleAssociation) *pb.RackRuleAssociation {
	if assoc == nil {
		return nil
	}

	return &pb.RackRuleAssociation{
		RackId:        UUIDTo(assoc.RackID),
		OperationType: OperationTypeToProto(assoc.OperationType),
		OperationCode: assoc.OperationCode,
		RuleId:        UUIDTo(assoc.RuleID),
		CreatedAt:     timestamppb.New(assoc.CreatedAt),
		UpdatedAt:     timestamppb.New(assoc.UpdatedAt),
	}
}

// RackRuleAssociationFromProto converts protobuf to domain object
func RackRuleAssociationFromProto(pbAssoc *pb.RackRuleAssociation) *operationrules.RackRuleAssociation {
	if pbAssoc == nil {
		return nil
	}

	assoc := &operationrules.RackRuleAssociation{
		RackID:        UUIDFrom(pbAssoc.GetRackId()),
		OperationType: OperationTypeFromProto(pbAssoc.GetOperationType()),
		OperationCode: pbAssoc.GetOperationCode(),
		RuleID:        UUIDFrom(pbAssoc.GetRuleId()),
	}

	if pbAssoc.GetCreatedAt() != nil {
		assoc.CreatedAt = pbAssoc.GetCreatedAt().AsTime()
	}
	if pbAssoc.GetUpdatedAt() != nil {
		assoc.UpdatedAt = pbAssoc.GetUpdatedAt().AsTime()
	}

	return assoc
}

// TargetSpecFrom converts a proto OperationTargetSpec to an internal operation.TargetSpec.
func TargetSpecFrom(ts *pb.OperationTargetSpec) (operation.TargetSpec, error) {
	if ts == nil {
		return operation.TargetSpec{}, fmt.Errorf("target_spec is required")
	}

	var spec operation.TargetSpec
	switch targets := ts.GetTargets().(type) {
	case *pb.OperationTargetSpec_Racks:
		if len(targets.Racks.GetTargets()) == 0 {
			return operation.TargetSpec{}, fmt.Errorf(
				"racks.targets must have at least one entry",
			)
		}
		for _, pbRack := range targets.Racks.GetTargets() {
			rt, err := RackTargetFrom(pbRack)
			if err != nil {
				return operation.TargetSpec{}, fmt.Errorf(
					"convert rack target: %w", err,
				)
			}
			spec.Racks = append(spec.Racks, rt)
		}
	case *pb.OperationTargetSpec_Components:
		if len(targets.Components.GetTargets()) == 0 {
			return operation.TargetSpec{}, fmt.Errorf(
				"components.targets must have at least one entry",
			)
		}
		for _, pbComp := range targets.Components.GetTargets() {
			ct, err := ComponentTargetFrom(pbComp)
			if err != nil {
				return operation.TargetSpec{}, fmt.Errorf(
					"convert component target: %w", err,
				)
			}
			spec.Components = append(spec.Components, ct)
		}
	case *pb.OperationTargetSpec_NvlDomains:
		if len(targets.NvlDomains.GetTargets()) == 0 {
			return operation.TargetSpec{}, fmt.Errorf(
				"nvl_domains.targets must have at least one entry",
			)
		}
		for _, pbDomain := range targets.NvlDomains.GetTargets() {
			dt, err := NVLDomainTargetFrom(pbDomain)
			if err != nil {
				return operation.TargetSpec{}, fmt.Errorf(
					"convert NVLink domain target: %w", err,
				)
			}
			spec.NVLDomains = append(spec.NVLDomains, dt)
		}
	default:
		return operation.TargetSpec{}, fmt.Errorf(
			"target_spec must have one of racks, nvl_domains, or components set",
		)
	}

	return spec, nil
}

// TargetSpecTo converts an internal operation.TargetSpec to its proto form.
// It returns an error unless exactly one target kind is populated, matching the
// mutual-exclusion rule enforced by TargetSpecFrom on the inbound path.
func TargetSpecTo(ts operation.TargetSpec) (*pb.OperationTargetSpec, error) {
	hasRacks := len(ts.Racks) > 0
	hasNVLDomains := len(ts.NVLDomains) > 0
	hasComponents := len(ts.Components) > 0

	targetKinds := 0
	for _, present := range []bool{hasRacks, hasNVLDomains, hasComponents} {
		if present {
			targetKinds++
		}
	}
	if targetKinds != 1 {
		return nil, fmt.Errorf("target_spec must have exactly one of racks, nvl_domains, or components set")
	}

	// Rack targets, converted to proto RackTargets.
	if hasRacks {
		racks := make([]*pb.RackTarget, 0, len(ts.Racks))
		for _, r := range ts.Racks {
			rt := &pb.RackTarget{}
			if r.Identifier.ExternalID != "" {
				rt.Identifier = &pb.RackTarget_ExternalId{
					ExternalId: r.Identifier.ExternalID,
				}
			} else if r.Identifier.ID != uuid.Nil {
				rt.Identifier = &pb.RackTarget_Id{
					Id: UUIDTo(r.Identifier.ID),
				}
			} else if r.Identifier.Name != "" {
				rt.Identifier = &pb.RackTarget_Name{
					Name: r.Identifier.Name,
				}
			} else {
				return nil, fmt.Errorf("invalid rack target: neither id, external_id, nor name is set")
			}

			for _, ct := range r.ComponentTypes {
				protoType := ComponentTypeTo(ct)
				if protoType == pb.ComponentType_COMPONENT_TYPE_UNKNOWN {
					return nil, fmt.Errorf(
						"invalid rack target: unknown component type filter",
					)
				}
				rt.ComponentTypes = append(rt.ComponentTypes, protoType)
			}

			racks = append(racks, rt)
		}

		return &pb.OperationTargetSpec{
			Targets: &pb.OperationTargetSpec_Racks{
				Racks: &pb.RackTargets{
					Targets: racks,
				},
			},
		}, nil
	}

	if hasNVLDomains {
		domains := make([]*pb.NVLDomainTarget, 0, len(ts.NVLDomains))
		for _, domain := range ts.NVLDomains {
			target := &pb.NVLDomainTarget{}
			if domain.Identifier.ID != uuid.Nil {
				target.Identifier = &pb.NVLDomainTarget_Id{
					Id: UUIDTo(domain.Identifier.ID),
				}
			} else if domain.Identifier.Name != "" {
				target.Identifier = &pb.NVLDomainTarget_Name{
					Name: domain.Identifier.Name,
				}
			} else {
				return nil, fmt.Errorf("invalid NVLink domain target: neither id nor name is set")
			}

			for _, componentType := range domain.ComponentTypes {
				protoType := ComponentTypeTo(componentType)
				if protoType == pb.ComponentType_COMPONENT_TYPE_UNKNOWN {
					return nil, fmt.Errorf(
						"invalid NVLink domain target: unknown component type filter",
					)
				}
				target.ComponentTypes = append(
					target.ComponentTypes,
					protoType,
				)
			}

			domains = append(domains, target)
		}

		return &pb.OperationTargetSpec{
			Targets: &pb.OperationTargetSpec_NvlDomains{
				NvlDomains: &pb.NVLDomainTargets{
					Targets: domains,
				},
			},
		}, nil
	}

	// Component targets, converted to proto ComponentTargets.
	comps := make([]*pb.ComponentTarget, 0, len(ts.Components))
	for _, c := range ts.Components {
		ct := &pb.ComponentTarget{}
		if c.UUID != uuid.Nil {
			ct.Identifier = &pb.ComponentTarget_Id{
				Id: UUIDTo(c.UUID),
			}
		} else if c.External != nil {
			ct.Identifier = &pb.ComponentTarget_External{
				External: &pb.ExternalRef{
					Type: ComponentTypeTo(c.External.Type),
					Id:   c.External.ID,
				},
			}
		} else {
			return nil, fmt.Errorf("invalid component target: neither uuid nor external ref is set")
		}

		comps = append(comps, ct)
	}

	return &pb.OperationTargetSpec{
		Targets: &pb.OperationTargetSpec_Components{
			Components: &pb.ComponentTargets{
				Targets: comps,
			},
		},
	}, nil
}

// NVLDomainTargetFrom converts a proto NVLink domain target to an internal target.
func NVLDomainTargetFrom(dt *pb.NVLDomainTarget) (operation.NVLDomainTarget, error) {
	if dt == nil {
		return operation.NVLDomainTarget{}, fmt.Errorf("NVLink domain target is nil")
	}

	var target operation.NVLDomainTarget
	switch id := dt.GetIdentifier().(type) {
	case *pb.NVLDomainTarget_Id:
		parsed, err := uuid.Parse(id.Id.GetId())
		if err != nil {
			return operation.NVLDomainTarget{}, fmt.Errorf(
				"invalid NVLink domain id %q: %w", id.Id.GetId(), err,
			)
		}
		target.Identifier.ID = parsed
	case *pb.NVLDomainTarget_Name:
		if id.Name == "" {
			return operation.NVLDomainTarget{}, fmt.Errorf("NVLink domain target name must not be empty")
		}
		target.Identifier.Name = id.Name
	default:
		return operation.NVLDomainTarget{}, fmt.Errorf(
			"NVLink domain target must have either id or name set",
		)
	}

	for _, pbType := range dt.GetComponentTypes() {
		componentType := ComponentTypeFrom(pbType)
		if componentType == devicetypes.ComponentTypeUnknown {
			return operation.NVLDomainTarget{}, fmt.Errorf(
				"unknown component type %v in NVLink domain target filter", pbType,
			)
		}
		target.ComponentTypes = append(target.ComponentTypes, componentType)
	}

	return target, nil
}

// RackTargetFrom converts a proto RackTarget to an internal operation.RackTarget.
func RackTargetFrom(rt *pb.RackTarget) (operation.RackTarget, error) {
	if rt == nil {
		return operation.RackTarget{}, fmt.Errorf("rack target is nil")
	}

	var target operation.RackTarget

	switch id := rt.GetIdentifier().(type) {
	case *pb.RackTarget_Id:
		rawID := id.Id.GetId()
		if rawID == "" {
			return operation.RackTarget{}, fmt.Errorf("rack target id must not be empty")
		}
		parsed, err := uuid.Parse(rawID)
		if err != nil {
			return operation.RackTarget{}, fmt.Errorf("invalid rack uuid %q: %w", rawID, err)
		}
		target.Identifier.ID = parsed
	case *pb.RackTarget_ExternalId:
		if id.ExternalId == "" {
			return operation.RackTarget{}, fmt.Errorf("rack target external_id must not be empty")
		}
		target.Identifier.ExternalID = id.ExternalId
	case *pb.RackTarget_Name:
		if id.Name == "" {
			return operation.RackTarget{}, fmt.Errorf("rack target name must not be empty")
		}
		target.Identifier.Name = id.Name
	default:
		return operation.RackTarget{}, fmt.Errorf("rack target must have either id, external_id, or name set")
	}

	for _, pbType := range rt.GetComponentTypes() {
		ct := ComponentTypeFrom(pbType)
		if ct == devicetypes.ComponentTypeUnknown {
			return operation.RackTarget{}, fmt.Errorf(
				"unknown component type %v in rack target filter", pbType,
			)
		}
		target.ComponentTypes = append(target.ComponentTypes, ct)
	}

	return target, nil
}

// ComponentTargetFrom converts a proto ComponentTarget to an internal operation.ComponentTarget.
func ComponentTargetFrom(ct *pb.ComponentTarget) (operation.ComponentTarget, error) {
	if ct == nil {
		return operation.ComponentTarget{}, fmt.Errorf("component target is nil")
	}

	var target operation.ComponentTarget

	switch id := ct.GetIdentifier().(type) {
	case *pb.ComponentTarget_Id:
		parsed, err := uuid.Parse(id.Id.GetId())
		if err != nil {
			return operation.ComponentTarget{}, fmt.Errorf("invalid component uuid %q: %w", id.Id.GetId(), err)
		}
		target.UUID = parsed
	case *pb.ComponentTarget_External:
		extType := ComponentTypeFrom(id.External.GetType())
		if id.External.GetId() == "" {
			return operation.ComponentTarget{}, fmt.Errorf("external component id must not be empty")
		}
		target.External = &operation.ExternalRef{
			Type: extType,
			ID:   id.External.GetId(),
		}
	default:
		return operation.ComponentTarget{}, fmt.Errorf("component target must have either uuid or external set")
	}

	return target, nil
}

// ScheduledOperationFrom converts a proto ScheduledOperation oneof to the
// internal Operation, TargetSpec, and request-level scheduling options. All
// values are always valid together: the Operation carries the task-type and
// parameters, the TargetSpec identifies the racks, NVLink domains, or components the task will
// run against, and the returned QueueOptions / rule UUID carry the caller's
// conflict-handling and rule-override preferences for use at fire time.
func ScheduledOperationFrom(
	scheduled *pb.ScheduledOperation,
) (operations.Operation, operation.TargetSpec, *pb.QueueOptions, *pb.UUID, error) {
	if scheduled == nil || scheduled.GetOperation() == nil {
		return nil, operation.TargetSpec{}, nil, nil, errors.New("operation is required")
	}

	switch r := scheduled.GetOperation().(type) {
	case *pb.ScheduledOperation_PowerOn:
		ts, err := TargetSpecFrom(r.PowerOn.GetTargetSpec())
		if err != nil {
			return nil, operation.TargetSpec{}, nil, nil, fmt.Errorf(
				"invalid target_spec: %w", err,
			)
		}

		return &operations.PowerControlTaskInfo{
			Operation:              operations.PowerOperationPowerOn,
			OverrideReadinessCheck: r.PowerOn.GetOverrideReadinessCheck(),
		}, ts, r.PowerOn.GetQueueOptions(), r.PowerOn.GetRuleId(), nil

	case *pb.ScheduledOperation_PowerOff:
		powerOp := operations.PowerOperationPowerOff
		if r.PowerOff.GetForced() {
			powerOp = operations.PowerOperationForcePowerOff
		}

		ts, err := TargetSpecFrom(r.PowerOff.GetTargetSpec())
		if err != nil {
			return nil, operation.TargetSpec{}, nil, nil, fmt.Errorf(
				"invalid target_spec: %w", err,
			)
		}

		return &operations.PowerControlTaskInfo{
			Operation:              powerOp,
			Forced:                 r.PowerOff.GetForced(),
			OverrideReadinessCheck: r.PowerOff.GetOverrideReadinessCheck(),
		}, ts, r.PowerOff.GetQueueOptions(), r.PowerOff.GetRuleId(), nil

	case *pb.ScheduledOperation_PowerReset:
		powerOp := operations.PowerOperationRestart
		if r.PowerReset.GetForced() {
			powerOp = operations.PowerOperationForceRestart
		}

		ts, err := TargetSpecFrom(r.PowerReset.GetTargetSpec())
		if err != nil {
			return nil, operation.TargetSpec{}, nil, nil, fmt.Errorf(
				"invalid target_spec: %w", err,
			)
		}

		return &operations.PowerControlTaskInfo{
			Operation:              powerOp,
			Forced:                 r.PowerReset.GetForced(),
			OverrideReadinessCheck: r.PowerReset.GetOverrideReadinessCheck(),
		}, ts, r.PowerReset.GetQueueOptions(), r.PowerReset.GetRuleId(), nil

	case *pb.ScheduledOperation_BringUp:
		ts, err := TargetSpecFrom(r.BringUp.GetTargetSpec())
		if err != nil {
			return nil, operation.TargetSpec{}, nil, nil, fmt.Errorf(
				"invalid target_spec: %w", err,
			)
		}

		return &operations.BringUpTaskInfo{
			OverrideReadinessCheck: r.BringUp.GetOverrideReadinessCheck(),
		}, ts, nil, r.BringUp.GetRuleId(), nil

	case *pb.ScheduledOperation_Ingest:
		ts, err := TargetSpecFrom(r.Ingest.GetTargetSpec())
		if err != nil {
			return nil, operation.TargetSpec{}, nil, nil, fmt.Errorf(
				"invalid target_spec: %w", err,
			)
		}

		return &operations.BringUpTaskInfo{OpCode: taskcommon.OpCodeIngest}, ts, nil, r.Ingest.GetRuleId(), nil

	case *pb.ScheduledOperation_UpgradeFirmware:
		info := &operations.FirmwareControlTaskInfo{
			Operation:              operations.FirmwareOperationUpgrade,
			TargetVersion:          r.UpgradeFirmware.GetTargetVersion(),
			SubTargets:             r.UpgradeFirmware.GetSubTargets(),
			OverrideReadinessCheck: r.UpgradeFirmware.GetOverrideReadinessCheck(),
		}

		if r.UpgradeFirmware.GetStartTime() != nil {
			info.StartTime = r.UpgradeFirmware.GetStartTime().AsTime().Unix()
		}

		if r.UpgradeFirmware.GetEndTime() != nil {
			info.EndTime = r.UpgradeFirmware.GetEndTime().AsTime().Unix()
		}

		ts, err := TargetSpecFrom(r.UpgradeFirmware.GetTargetSpec())
		if err != nil {
			return nil, operation.TargetSpec{}, nil, nil, fmt.Errorf(
				"invalid target_spec: %w", err,
			)
		}

		return info, ts, r.UpgradeFirmware.GetQueueOptions(), r.UpgradeFirmware.GetRuleId(), nil

	default:
		// Unreachable with well-typed proto code: all
		// isScheduledOperation_Operation implementations are generated types
		// with explicit cases above. This fires only if a new oneof variant
		// is added to the proto without updating this switch.
		return nil, operation.TargetSpec{}, nil, nil, errors.New(
			"unsupported scheduled operation type",
		)
	}
}

// QueueOptionsFrom converts a proto QueueOptions message to the two fields
// used on operation.Request. A nil opts is handled safely — both return
// values will be their zero values (reject on conflict, server default timeout).
func QueueOptionsFrom(
	opts *pb.QueueOptions,
) (strategy operation.ConflictStrategy, timeout time.Duration) {
	if opts == nil {
		return operation.ConflictStrategyReject, 0
	}

	if s := opts.GetQueueTimeoutSeconds(); s > 0 {
		timeout = time.Duration(s) * time.Second
	}

	switch opts.GetConflictStrategy() {
	case pb.ConflictStrategy_CONFLICT_STRATEGY_QUEUE:
		return operation.ConflictStrategyQueue, timeout
	default:
		return operation.ConflictStrategyReject, timeout
	}
}
