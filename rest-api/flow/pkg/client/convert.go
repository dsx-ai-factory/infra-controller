// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package client

import (
	"net"

	"github.com/google/uuid"

	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
)

// Proto to types conversions

func rackFromProto(r *pb.Rack) *types.Rack {
	if r == nil {
		return nil
	}

	rack := &types.Rack{
		Info:            deviceInfoFromProto(r.GetInfo()),
		Location:        locationFromProto(r.GetLocation()),
		NVLDomainIDs:    uuidsFromProto(r.GetNvlDomainIds()),
		OperationStatus: phaseFromProto(r.GetOperationStatus()),
	}

	if len(r.GetComponents()) > 0 {
		rack.Components = make([]types.Component, 0, len(r.GetComponents()))
		for _, c := range r.GetComponents() {
			rack.Components = append(rack.Components, *componentFromProto(c))
		}
	}

	return rack
}

func componentFromProto(c *pb.Component) *types.Component {
	if c == nil {
		return nil
	}

	comp := &types.Component{
		Type:            componentTypeFromProto(c.GetType()),
		Info:            deviceInfoFromProto(c.GetInfo()),
		FirmwareVersion: c.GetFirmwareVersion(),
		Position:        positionFromProto(c.GetPosition()),
		ComponentID:     c.GetComponentId(),
		RackID:          uuidFromProto(c.GetRackId()),
		NVLDomainID:     uuidFromProto(c.GetNvlDomainId()),
		PowerState:      c.GetPowerState(),
	}

	if len(c.GetBmcs()) > 0 {
		comp.BMCs = make([]types.BMC, 0, len(c.GetBmcs()))
		for _, b := range c.GetBmcs() {
			comp.BMCs = append(comp.BMCs, bmcFromProto(b))
		}
	}

	return comp
}

func deviceInfoFromProto(info *pb.DeviceInfo) types.DeviceInfo {
	if info == nil {
		return types.DeviceInfo{}
	}

	return types.DeviceInfo{
		ID:           uuidFromProto(info.GetId()),
		Name:         info.GetName(),
		Manufacturer: info.GetManufacturer(),
		Model:        info.GetModel(),
		SerialNumber: info.GetSerialNumber(),
		Description:  info.GetDescription(),
	}
}

func locationFromProto(loc *pb.Location) types.Location {
	if loc == nil {
		return types.Location{}
	}

	return types.Location{
		Region:     loc.GetRegion(),
		Datacenter: loc.GetDatacenter(),
		Room:       loc.GetRoom(),
		Position:   loc.GetPosition(),
	}
}

func positionFromProto(pos *pb.RackPosition) types.InRackPosition {
	if pos == nil {
		return types.InRackPosition{}
	}

	return types.InRackPosition{
		SlotID:    int(pos.GetSlotId()),
		TrayIndex: int(pos.GetTrayIdx()),
		HostID:    int(pos.GetHostId()),
	}
}

func bmcFromProto(b *pb.BMCInfo) types.BMC {
	if b == nil {
		return types.BMC{}
	}

	bmc := types.BMC{
		Type: bmcTypeFromProto(b.GetType()),
	}

	// Invalid MAC addresses are silently ignored; bmc.MAC remains unset (nil).
	// This matches the behaviour of the internal DAO and protobuf converters.
	if mac := b.GetMacAddress(); mac != "" {
		if addr, err := net.ParseMAC(mac); err == nil {
			bmc.MAC = addr
		}
	}

	if ip := b.GetIpAddress(); ip != "" {
		bmc.IP = net.ParseIP(ip)
	}

	return bmc
}

func uuidFromProto(u *pb.UUID) uuid.UUID {
	if u == nil || u.GetId() == "" {
		return uuid.Nil
	}
	id, _ := uuid.Parse(u.GetId())
	return id
}

func uuidsFromProto(uuids []*pb.UUID) []uuid.UUID {
	result := make([]uuid.UUID, 0, len(uuids))
	for _, u := range uuids {
		result = append(result, uuidFromProto(u))
	}
	return result
}

func nvlDomainFromProto(d *pb.NVLDomain) *types.NVLDomain {
	if d == nil {
		return nil
	}

	domain := &types.NVLDomain{}
	if id := d.GetIdentifier(); id != nil {
		domain.ID = uuidFromProto(id.GetId())
		domain.Name = id.GetName()
	}
	return domain
}

func taskFromProto(t *pb.Task) *types.Task {
	if t == nil {
		return nil
	}

	return &types.Task{
		ID:           uuidFromProto(t.GetId()),
		Operation:    t.GetOperation(),
		RackID:       uuidFromProto(t.GetRackId()),
		ComponentIDs: uuidsFromProto(t.GetComponentUuids()),
		Description:  t.GetDescription(),
		ExecutorType: taskExecutorTypeFromProto(t.GetExecutorType()),
		ExecutionID:  t.GetExecutionId(),
		Status:       taskStatusFromProto(t.GetStatus()),
		Message:      t.GetMessage(),
	}
}

func componentDiffFromProto(d *pb.ComponentDiff) *types.ComponentDiff {
	if d == nil {
		return nil
	}

	diff := &types.ComponentDiff{
		Type:        diffTypeFromProto(d.GetType()),
		ID:          uuidFromProto(d.GetId()),
		ComponentID: d.GetComponentId(),
		Expected:    componentFromProto(d.GetExpected()),
		Actual:      componentFromProto(d.GetActual()),
	}

	if len(d.GetFieldDiffs()) > 0 {
		diff.FieldDiffs = make([]types.FieldDiff, 0, len(d.GetFieldDiffs()))
		for _, fd := range d.GetFieldDiffs() {
			diff.FieldDiffs = append(diff.FieldDiffs, types.FieldDiff{
				FieldName:     fd.GetFieldName(),
				ExpectedValue: fd.GetExpectedValue(),
				ActualValue:   fd.GetActualValue(),
			})
		}
	}

	return diff
}

// Enum conversions from proto

func componentTypeFromProto(ct pb.ComponentType) types.ComponentType {
	switch ct {
	case pb.ComponentType_COMPONENT_TYPE_COMPUTE:
		return types.ComponentTypeCompute
	case pb.ComponentType_COMPONENT_TYPE_NVSWITCH:
		return types.ComponentTypeNVSwitch
	case pb.ComponentType_COMPONENT_TYPE_POWERSHELF:
		return types.ComponentTypePowerShelf
	case pb.ComponentType_COMPONENT_TYPE_TORSWITCH:
		return types.ComponentTypeTORSwitch
	case pb.ComponentType_COMPONENT_TYPE_UMS:
		return types.ComponentTypeUMS
	case pb.ComponentType_COMPONENT_TYPE_CDU:
		return types.ComponentTypeCDU
	default:
		return types.ComponentTypeUnknown
	}
}

func bmcTypeFromProto(bt pb.BMCType) types.BMCType {
	switch bt {
	case pb.BMCType_BMC_TYPE_HOST:
		return types.BMCTypeHost
	case pb.BMCType_BMC_TYPE_DPU:
		return types.BMCTypeDPU
	default:
		return types.BMCTypeUnknown
	}
}

func taskStatusFromProto(ts pb.TaskStatus) types.TaskStatus {
	switch ts {
	case pb.TaskStatus_TASK_STATUS_PENDING:
		return types.TaskStatusPending
	case pb.TaskStatus_TASK_STATUS_RUNNING:
		return types.TaskStatusRunning
	case pb.TaskStatus_TASK_STATUS_COMPLETED:
		return types.TaskStatusCompleted
	case pb.TaskStatus_TASK_STATUS_FAILED:
		return types.TaskStatusFailed
	default:
		return types.TaskStatusUnknown
	}
}

func taskExecutorTypeFromProto(et pb.TaskExecutorType) types.TaskExecutorType {
	switch et {
	case pb.TaskExecutorType_TASK_EXECUTOR_TYPE_TEMPORAL:
		return types.TaskExecutorTypeTemporal
	default:
		return types.TaskExecutorTypeUnknown
	}
}

func diffTypeFromProto(dt pb.DiffType) types.DiffType {
	switch dt {
	case pb.DiffType_DIFF_TYPE_MISSING:
		return types.DiffTypeMissing
	case pb.DiffType_DIFF_TYPE_UNEXPECTED:
		return types.DiffTypeUnexpected
	case pb.DiffType_DIFF_TYPE_MISMATCH:
		return types.DiffTypeMismatch
	default:
		return types.DiffTypeUnknown
	}
}

func phaseFromProto(phase pb.Phase) types.Phase {
	switch phase {
	case pb.Phase_PHASE_INITIALIZING:
		return types.PhaseInitializing
	case pb.Phase_PHASE_READY:
		return types.PhaseReady
	case pb.Phase_PHASE_IN_USE:
		return types.PhaseInUse
	case pb.Phase_PHASE_ERROR:
		return types.PhaseError
	case pb.Phase_PHASE_DELETING:
		return types.PhaseDeleting
	default:
		return types.PhaseUnknown
	}
}

// Types to proto conversions

func rackToProto(r *types.Rack) *pb.Rack {
	if r == nil {
		return nil
	}

	rack := &pb.Rack{
		Info:     deviceInfoToProto(&r.Info),
		Location: locationToProto(&r.Location),
	}
	// OperationStatus is deliberately omitted: this conversion builds a
	// CreateExpectedRack request, while Flow owns the derived response field.
	rack.NvlDomainIds = uuidsToProto(r.NVLDomainIDs)

	if len(r.Components) > 0 {
		rack.Components = make([]*pb.Component, 0, len(r.Components))
		for _, c := range r.Components {
			rack.Components = append(rack.Components, componentToProto(&c))
		}
	}

	return rack
}

func componentToProto(c *types.Component) *pb.Component {
	if c == nil {
		return nil
	}

	comp := &pb.Component{
		Type:            componentTypeToProto(c.Type),
		Info:            deviceInfoToProto(&c.Info),
		FirmwareVersion: c.FirmwareVersion,
		Position:        positionToProto(&c.Position),
		ComponentId:     c.ComponentID,
		RackId:          uuidToProto(c.RackID),
		NvlDomainId:     uuidToProto(c.NVLDomainID),
	}

	if len(c.BMCs) > 0 {
		comp.Bmcs = make([]*pb.BMCInfo, 0, len(c.BMCs))
		for _, b := range c.BMCs {
			comp.Bmcs = append(comp.Bmcs, bmcToProto(&b))
		}
	}

	return comp
}

func deviceInfoToProto(info *types.DeviceInfo) *pb.DeviceInfo {
	if info == nil {
		return nil
	}

	return &pb.DeviceInfo{
		Id:           uuidToProto(info.ID),
		Name:         info.Name,
		Manufacturer: info.Manufacturer,
		Model:        &info.Model,
		SerialNumber: info.SerialNumber,
		Description:  &info.Description,
	}
}

func locationToProto(loc *types.Location) *pb.Location {
	if loc == nil {
		return nil
	}

	return &pb.Location{
		Region:     loc.Region,
		Datacenter: loc.Datacenter,
		Room:       loc.Room,
		Position:   loc.Position,
	}
}

func positionToProto(pos *types.InRackPosition) *pb.RackPosition {
	if pos == nil {
		return nil
	}

	return &pb.RackPosition{
		SlotId:  int32(pos.SlotID),
		TrayIdx: int32(pos.TrayIndex),
		HostId:  int32(pos.HostID),
	}
}

func bmcToProto(b *types.BMC) *pb.BMCInfo {
	if b == nil {
		return nil
	}

	info := &pb.BMCInfo{
		Type: bmcTypeToProto(b.Type),
	}

	if b.MAC != nil {
		mac := b.MAC.String()
		info.MacAddress = mac
	}

	if b.IP != nil {
		ip := b.IP.String()
		info.IpAddress = &ip
	}

	return info
}

func uuidToProto(id uuid.UUID) *pb.UUID {
	if id == uuid.Nil {
		return nil
	}
	return &pb.UUID{Id: id.String()}
}

func uuidsToProto(ids []uuid.UUID) []*pb.UUID {
	result := make([]*pb.UUID, 0, len(ids))
	for _, id := range ids {
		result = append(result, uuidToProto(id))
	}
	return result
}

func nvlDomainToProto(d *types.NVLDomain) *pb.NVLDomain {
	if d == nil {
		return nil
	}

	return &pb.NVLDomain{
		Identifier: &pb.Identifier{
			Id:   uuidToProto(d.ID),
			Name: d.Name,
		},
	}
}

func identifierToProto(id *types.Identifier) *pb.Identifier {
	if id == nil {
		return nil
	}
	return &pb.Identifier{
		Id:   uuidToProto(id.ID),
		Name: id.Name,
	}
}

func paginationToProto(p *types.Pagination) *pb.Pagination {
	if p == nil {
		return nil
	}
	return &pb.Pagination{
		Offset: int32(p.Offset),
		Limit:  int32(p.Limit),
	}
}

func stringQueryInfoToProto(s *types.StringQueryInfo) *pb.StringQueryInfo {
	if s == nil {
		return nil
	}
	return &pb.StringQueryInfo{
		Patterns:   s.Patterns,
		IsWildcard: s.IsWildcard,
		UseOr:      s.UseOR,
	}
}

// Enum conversions to proto

func componentTypeToProto(ct types.ComponentType) pb.ComponentType {
	switch ct {
	case types.ComponentTypeCompute:
		return pb.ComponentType_COMPONENT_TYPE_COMPUTE
	case types.ComponentTypeNVSwitch:
		return pb.ComponentType_COMPONENT_TYPE_NVSWITCH
	case types.ComponentTypePowerShelf:
		return pb.ComponentType_COMPONENT_TYPE_POWERSHELF
	case types.ComponentTypeTORSwitch:
		return pb.ComponentType_COMPONENT_TYPE_TORSWITCH
	case types.ComponentTypeUMS:
		return pb.ComponentType_COMPONENT_TYPE_UMS
	case types.ComponentTypeCDU:
		return pb.ComponentType_COMPONENT_TYPE_CDU
	default:
		return pb.ComponentType_COMPONENT_TYPE_UNKNOWN
	}
}

// componentTypesFilter returns a []pb.ComponentType restricting to ct, or nil
// when ct is ComponentTypeUnknown (meaning "all component types").
func componentTypesFilter(ct types.ComponentType) []pb.ComponentType {
	if ct == types.ComponentTypeUnknown {
		return nil
	}

	return []pb.ComponentType{componentTypeToProto(ct)}
}

// componentTypeToString converts types.ComponentType to its string representation
// for use in filters map
func componentTypeToString(ct types.ComponentType) string {
	switch ct {
	case types.ComponentTypeCompute:
		return "Compute"
	case types.ComponentTypeNVSwitch:
		return "NVSwitch"
	case types.ComponentTypePowerShelf:
		return "PowerShelf"
	case types.ComponentTypeTORSwitch:
		return "ToRSwitch"
	case types.ComponentTypeUMS:
		return "UMS"
	case types.ComponentTypeCDU:
		return "CDU"
	default:
		return "Unknown"
	}
}

func bmcTypeToProto(bt types.BMCType) pb.BMCType {
	switch bt {
	case types.BMCTypeHost:
		return pb.BMCType_BMC_TYPE_HOST
	case types.BMCTypeDPU:
		return pb.BMCType_BMC_TYPE_DPU
	default:
		return pb.BMCType_BMC_TYPE_UNKNOWN
	}
}

func powerControlOpToProto(op types.PowerControlOp) pb.PowerControlOp {
	switch op {
	case types.PowerControlOpOn:
		return pb.PowerControlOp_POWER_CONTROL_OP_ON
	case types.PowerControlOpForceOn:
		return pb.PowerControlOp_POWER_CONTROL_OP_FORCE_ON
	case types.PowerControlOpOff:
		return pb.PowerControlOp_POWER_CONTROL_OP_OFF
	case types.PowerControlOpForceOff:
		return pb.PowerControlOp_POWER_CONTROL_OP_FORCE_OFF
	case types.PowerControlOpRestart:
		return pb.PowerControlOp_POWER_CONTROL_OP_RESTART
	case types.PowerControlOpForceRestart:
		return pb.PowerControlOp_POWER_CONTROL_OP_FORCE_RESTART
	case types.PowerControlOpWarmReset:
		return pb.PowerControlOp_POWER_CONTROL_OP_WARM_RESET
	case types.PowerControlOpColdReset:
		return pb.PowerControlOp_POWER_CONTROL_OP_COLD_RESET
	default:
		return pb.PowerControlOp_POWER_CONTROL_OP_UNKNOWN
	}
}

func operationTypeFromProto(ot pb.OperationType) types.OperationType {
	switch ot {
	case pb.OperationType_OPERATION_TYPE_POWER_CONTROL:
		return types.OperationTypePowerControl
	case pb.OperationType_OPERATION_TYPE_FIRMWARE_CONTROL:
		return types.OperationTypeFirmwareControl
	default:
		return types.OperationTypeUnknown
	}
}

func operationTypeToProto(ot types.OperationType) pb.OperationType {
	switch ot {
	case types.OperationTypePowerControl:
		return pb.OperationType_OPERATION_TYPE_POWER_CONTROL
	case types.OperationTypeFirmwareControl:
		return pb.OperationType_OPERATION_TYPE_FIRMWARE_CONTROL
	default:
		return pb.OperationType_OPERATION_TYPE_UNKNOWN
	}
}

func operationRuleFromProto(r *pb.OperationRule) *types.OperationRule {
	if r == nil {
		return nil
	}

	rule := &types.OperationRule{
		ID:                 uuidFromProto(r.GetId()),
		Name:               r.GetName(),
		Description:        r.GetDescription(),
		OperationType:      operationTypeFromProto(r.GetOperationType()),
		OperationCode:      r.GetOperationCode(),
		RuleDefinitionJSON: r.GetRuleDefinitionJson(),
		IsDefault:          r.GetIsDefault(),
	}

	if r.GetCreatedAt() != nil {
		rule.CreatedAt = r.GetCreatedAt().AsTime()
	}
	if r.GetUpdatedAt() != nil {
		rule.UpdatedAt = r.GetUpdatedAt().AsTime()
	}

	return rule
}

func rackRuleAssociationFromProto(a *pb.RackRuleAssociation) *types.RackRuleAssociation {
	if a == nil {
		return nil
	}

	assoc := &types.RackRuleAssociation{
		RackID:        uuidFromProto(a.GetRackId()),
		OperationType: operationTypeFromProto(a.GetOperationType()),
		OperationCode: a.GetOperationCode(),
		RuleID:        uuidFromProto(a.GetRuleId()),
	}

	if a.GetCreatedAt() != nil {
		assoc.CreatedAt = a.GetCreatedAt().AsTime()
	}
	if a.GetUpdatedAt() != nil {
		assoc.UpdatedAt = a.GetUpdatedAt().AsTime()
	}

	return assoc
}
