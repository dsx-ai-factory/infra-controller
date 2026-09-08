// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package dao

import (
	"database/sql"
	"encoding/json"
	"fmt"
	"net"

	"github.com/google/uuid"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/credential"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/nicoapi"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	operationrun "github.com/NVIDIA/infra-controller/rest-api/flow/internal/operationrun"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operationrules"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	taskdef "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	identifier "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/Identifier"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/deviceinfo"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/errors"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/location"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/utils"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/bmc"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/component"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/nvldomain"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/rack"
)

// BMCTypeFrom converts BMC type from DAO model to internal model.
func BMCTypeFrom(dt string) devicetypes.BMCType {
	return devicetypes.BMCTypeFromString(dt)
}

// ComponentTypeFrom convers component type from DAO model to internal model.
func ComponentTypeFrom(dt string) devicetypes.ComponentType {
	return devicetypes.ComponentTypeFromString(dt)
}

// powerStateFromDAO converts a nicoapi.PowerState pointer to a string.
func powerStateFromDAO(ps *nicoapi.PowerState) string {
	if ps == nil {
		return ""
	}
	switch *ps {
	case nicoapi.PowerStateOn:
		return "on"
	case nicoapi.PowerStateOff:
		return "off"
	case nicoapi.PowerStateDisabled:
		return "disabled"
	case nicoapi.PowerStateHibernating:
		return "hibernating"
	case nicoapi.PowerStateSleeping:
		return "sleeping"
	default:
		return "unknown"
	}
}

// BMCFrom converts BMC from DAO model to internal model
func BMCFrom(dao model.BMC) bmc.BMC {
	var b bmc.BMC

	if hwAddr, err := net.ParseMAC(dao.MacAddress); err == nil {
		b.MAC = bmc.MACAddress{HardwareAddr: hwAddr}
	}

	if dao.IPAddress != nil {
		b.IP = net.ParseIP(*dao.IPAddress)
	}

	if dao.User != nil && dao.Password != nil {
		// Create a new credential only if the user and password are not nil.
		nc := credential.New(*dao.User, *dao.Password)
		b.Credential = &nc
	}

	return b
}

// ComponentFrom converts Component from DAO model to internal model.
func ComponentFrom(dao model.Component) *component.Component {
	bmcsByType := make(map[devicetypes.BMCType][]bmc.BMC)
	for _, bd := range dao.BMCs {
		t := BMCTypeFrom(bd.Type)
		bmcsByType[t] = append(bmcsByType[t], BMCFrom(bd))
	}

	var nvlDomainID uuid.UUID
	if dao.Rack != nil && dao.Rack.NVLDomainID != uuid.Nil {
		nvlDomainID = dao.Rack.NVLDomainID
	}

	rackExternalID := ""
	if dao.Rack != nil && dao.Rack.ExternalID != nil {
		rackExternalID = *dao.Rack.ExternalID
	}

	return &component.Component{
		Type: ComponentTypeFrom(dao.Type),
		Info: deviceinfo.DeviceInfo{
			ID:           dao.ID,
			Name:         dao.Name,
			Manufacturer: dao.Manufacturer,
			Model:        dao.Model,
			SerialNumber: dao.SerialNumber,
			Description:  utils.MapToJSONString(dao.Description),
		},
		FirmwareVersion: dao.FirmwareVersion,
		Position: component.InRackPosition{
			SlotID:    dao.SlotID,
			TrayIndex: dao.TrayIndex,
			HostID:    dao.HostID,
		},
		BmcsByType:     bmcsByType,
		ComponentID:    cutil.GetValueOrZero(dao.ComponentID),
		RackID:         dao.RackID,
		RackExternalID: rackExternalID,
		NVLDomainID:    nvlDomainID,
		PowerState:     powerStateFromDAO(dao.PowerState),
		Status:         dao.Status,
		LeakStatus:     dao.LeakStatus,
	}
}

// RackFrom converts Rack from DAO model to internal model.
func RackFrom(dao *model.Rack) *rack.Rack {
	if dao == nil {
		return nil
	}
	modelName, description := rackMetadataFromDescription(dao.Description)

	components := make([]component.Component, 0, len(dao.Components))
	for _, c := range dao.Components {
		converted := ComponentFrom(c)
		converted.RackExternalID = cutil.GetValueOrZero(dao.ExternalID)
		if dao.NVLDomainID != uuid.Nil {
			converted.NVLDomainID = dao.NVLDomainID
		}
		components = append(components, *converted)
	}

	return &rack.Rack{
		Info: deviceinfo.DeviceInfo{
			ID:           dao.ID,
			Name:         dao.Name,
			Manufacturer: dao.Manufacturer,
			Model:        modelName,
			SerialNumber: dao.SerialNumber,
			Description:  description,
		},
		ExternalID: cutil.GetValueOrZero(dao.ExternalID),
		Loc: location.New(
			[]byte(utils.MapToJSONString(dao.Location)),
		),
		Components:  components,
		NVLDomainID: dao.NVLDomainID,
	}
}

// rackMetadataFromDescription separates the rack fields stored in the shared
// description JSONB column. Model has a dedicated public field, while the
// standard scalar description keys should be exposed as plain text rather than
// leaking the database JSON representation. Unknown structured metadata stays
// JSON so legacy values continue to round-trip.
func rackMetadataFromDescription(stored map[string]any) (string, string) {
	modelName, _ := stored["model"].(string)
	publicDescription := make(map[string]any, len(stored))
	for key, value := range stored {
		if key != "model" {
			publicDescription[key] = value
		}
	}
	if len(publicDescription) == 0 {
		return modelName, ""
	}

	if len(publicDescription) == 1 {
		for _, key := range []string{"text", "description"} {
			if value, ok := publicDescription[key].(string); ok {
				return modelName, value
			}
		}
	}

	return modelName, utils.MapToJSONString(publicDescription)
}

// NVLDomainFrom converts a DAO NVLDomain model to its domain object.
func NVLDomainFrom(dao *model.NVLDomain) *nvldomain.NVLDomain {
	if dao == nil {
		return nil
	}

	return &nvldomain.NVLDomain{
		Identifier: *identifier.New(dao.ID, dao.Name),
	}
}

// TaskFrom converts a persisted task to its domain representation and rejects
// invalid trigger metadata.
func TaskFrom(dao *model.Task) (*taskdef.Task, error) {
	if dao == nil {
		return nil, nil
	}

	triggerType, err := operation.TriggerTypeFromString(dao.TriggerType)
	if err != nil {
		return nil, fmt.Errorf("invalid persisted task trigger: %w", err)
	}
	if err := operation.ValidateTrigger(triggerType, dao.TriggerID); err != nil {
		return nil, fmt.Errorf("invalid persisted task trigger: %w", err)
	}

	// Extract operation code from the serialized information
	operationCode := ""
	if op, err := operations.New(dao.Type, dao.Information); err == nil {
		operationCode = op.CodeString()
	}

	return &taskdef.Task{
		ID: dao.ID,
		Operation: operation.Wrapper{
			Type: dao.Type,
			Code: operationCode,
			Info: dao.Information,
		},
		RackID:         dao.RackID,
		Attributes:     dao.Attributes,
		Description:    dao.Description,
		ExecutorType:   dao.ExecutorType,
		ExecutionID:    dao.ExecutionID,
		Status:         dao.Status,
		Message:        dao.Message,
		Report:         dao.Report,
		AppliedRuleID:  dao.AppliedRuleID,
		CreatedAt:      dao.CreatedAt,
		UpdatedAt:      dao.UpdatedAt,
		StartedAt:      dao.StartedAt,
		FinishedAt:     dao.FinishedAt,
		QueueExpiresAt: dao.QueueExpiresAt,
		TriggerType:    triggerType,
		TriggerID:      dao.TriggerID,
		IdempotencyKey: dao.IdempotencyKey,
	}, nil
}

// BMCTypeTo converts BMC type from internal model to DAO model
func BMCTypeTo(bt devicetypes.BMCType) string {
	return devicetypes.BMCTypeToString(bt)
}

// ComponentTypeTo converts Component type from internal model to DAO model
func ComponentTypeTo(ct devicetypes.ComponentType) string {
	return devicetypes.ComponentTypeToString(ct)
}

// BMCTo converts BMC from internal model to DAO model
func BMCTo(typ devicetypes.BMCType, b *bmc.BMC, compDao *model.Component) *model.BMC {
	if b == nil {
		return nil
	}

	bmcDAO := model.BMC{
		MacAddress: b.MAC.String(),
		Type:       BMCTypeTo(typ),
		Component:  compDao,
	}

	if compDao != nil {
		bmcDAO.ComponentID = compDao.ID
	} else {
		bmcDAO.ComponentID = uuid.Nil
	}

	if b.IP != nil {
		ip := b.IP.String()
		bmcDAO.IPAddress = &ip
	}

	if b.Credential != nil {
		bmcDAO.User, bmcDAO.Password = b.Credential.Retrieve()
	}

	return &bmcDAO
}

// ComponentTo converts Component from internal model to DAO model
func ComponentTo(c *component.Component, rackID uuid.UUID) *model.Component {
	if c == nil {
		return nil
	}

	compDAO := model.Component{
		ID:              c.Info.ID,
		Name:            c.Info.Name,
		Type:            ComponentTypeTo(c.Type),
		Manufacturer:    c.Info.Manufacturer,
		Model:           c.Info.Model,
		SerialNumber:    c.Info.SerialNumber,
		Description:     utils.JSONStringToMap("description", c.Info.Description),
		FirmwareVersion: c.FirmwareVersion,
		SlotID:          c.Position.SlotID,
		TrayIndex:       c.Position.TrayIndex,
		HostID:          c.Position.HostID,
		RackID:          rackID,
		ComponentID:     cutil.GetPtrIfNotZero(c.ComponentID),
	}

	for _, t := range devicetypes.BMCTypes() {
		for _, b := range c.BmcsByType[t] {
			bmcDAO := BMCTo(t, &b, &compDAO)
			compDAO.BMCs = append(compDAO.BMCs, *bmcDAO)
		}
	}

	return &compDAO
}

// RackTo converts Rack from internal model to DAO model
func RackTo(r *rack.Rack) *model.Rack {
	if r == nil {
		return nil
	}

	components := make([]model.Component, 0, len(r.Components))
	for _, c := range r.Components {
		components = append(components, *ComponentTo(&c, r.Info.ID))
	}

	description := utils.JSONStringToMap("description", r.Info.Description)
	if r.Info.Model != "" {
		if description == nil {
			description = make(map[string]any)
		}
		description["model"] = r.Info.Model
	}

	return &model.Rack{
		ID:           r.Info.ID,
		ExternalID:   cutil.GetPtrIfNotZero(r.ExternalID),
		Name:         r.Info.Name,
		Manufacturer: r.Info.Manufacturer,
		SerialNumber: r.Info.SerialNumber,
		Description:  description,
		Location:     r.Loc.ToMap(),
		Components:   components,
		NVLDomainID:  r.NVLDomainID,
	}
}

// NVLDomainTo converts NVLDomain from internal model to DAO model
func NVLDomainTo(n *nvldomain.NVLDomain) *model.NVLDomain {
	if n == nil {
		return nil
	}

	return &model.NVLDomain{
		ID:   n.Identifier.ID,
		Name: n.Identifier.Name,
	}
}

// TaskTo converts a task domain object to its DAO model.
func TaskTo(task *taskdef.Task) *model.Task {
	if task == nil {
		return nil
	}

	return &model.Task{
		ID:             task.ID,
		Type:           task.Operation.Type,
		Information:    task.Operation.Info,
		Description:    task.Description,
		RackID:         task.RackID,
		Attributes:     task.Attributes,
		ExecutorType:   task.ExecutorType,
		ExecutionID:    task.ExecutionID,
		Status:         task.Status,
		Message:        task.Message,
		Report:         task.Report,
		AppliedRuleID:  task.AppliedRuleID,
		QueueExpiresAt: task.QueueExpiresAt,
		TriggerType:    string(task.TriggerType),
		TriggerID:      task.TriggerID,
		IdempotencyKey: task.IdempotencyKey,
	}
}

// OperationRunFrom converts an operation-run DAO model to its domain object.
func OperationRunFrom(dao *model.OperationRun) *operationrun.OperationRun {
	if dao == nil {
		return nil
	}

	return &operationrun.OperationRun{
		ID:                dao.ID,
		Name:              dao.Name,
		Description:       dao.Description,
		Status:            dao.Status,
		StatusReason:      dao.StatusReason,
		StatusMessage:     dao.StatusMessage,
		CurrentPhaseIndex: dao.CurrentPhaseIndex,
		TotalPhases:       dao.TotalPhases,
		Selector:          dao.Selector,
		Options:           dao.Options,
		OperationTemplate: dao.OperationTemplate,
		OperationType:     dao.OperationType,
		OperationCode:     dao.OperationCode,
		CreatedAt:         dao.CreatedAt,
		UpdatedAt:         dao.UpdatedAt,
		StartedAt:         dao.StartedAt,
		FinishedAt:        dao.FinishedAt,
	}
}

// OperationRunTo converts an operation-run domain object to its DAO model.
func OperationRunTo(run *operationrun.OperationRun) *model.OperationRun {
	if run == nil {
		return nil
	}

	return &model.OperationRun{
		ID:                run.ID,
		Name:              run.Name,
		Description:       run.Description,
		Status:            run.Status,
		StatusReason:      run.StatusReason,
		StatusMessage:     run.StatusMessage,
		CurrentPhaseIndex: run.CurrentPhaseIndex,
		TotalPhases:       run.TotalPhases,
		Selector:          run.Selector,
		Options:           run.Options,
		OperationTemplate: run.OperationTemplate,
		OperationType:     run.OperationType,
		OperationCode:     run.OperationCode,
		CreatedAt:         run.CreatedAt,
		UpdatedAt:         run.UpdatedAt,
		StartedAt:         run.StartedAt,
		FinishedAt:        run.FinishedAt,
	}
}

// OperationRunTargetFrom converts an operation-run target DAO model to its
// domain object.
func OperationRunTargetFrom(dao *model.OperationRunTarget) *operationrun.OperationRunTarget {
	if dao == nil {
		return nil
	}

	return &operationrun.OperationRunTarget{
		ID:               dao.ID,
		OperationRunID:   dao.OperationRunID,
		RackID:           dao.RackID,
		SequenceIndex:    dao.SequenceIndex,
		PhaseIndex:       dao.PhaseIndex,
		ComponentsByType: dao.ComponentsByType.Clone(),
		TaskID:           dao.TaskID,
		Status:           dao.Status,
		Message:          dao.Message,
		RetryAfter:       dao.RetryAfter,
		RetryState:       dao.RetryState,
		CreatedAt:        dao.CreatedAt,
		UpdatedAt:        dao.UpdatedAt,
	}
}

// OperationRunTargetTo converts an operation-run target domain object to its
// DAO model.
func OperationRunTargetTo(target *operationrun.OperationRunTarget) *model.OperationRunTarget {
	if target == nil {
		return nil
	}

	return &model.OperationRunTarget{
		ID:               target.ID,
		OperationRunID:   target.OperationRunID,
		RackID:           target.RackID,
		SequenceIndex:    target.SequenceIndex,
		PhaseIndex:       target.PhaseIndex,
		ComponentsByType: target.ComponentsByType.Clone(),
		TaskID:           target.TaskID,
		Status:           target.Status,
		Message:          target.Message,
		RetryAfter:       target.RetryAfter,
		RetryState:       target.RetryState,
		CreatedAt:        target.CreatedAt,
		UpdatedAt:        target.UpdatedAt,
	}
}

// OperationRuleTo converts domain object to database model
func OperationRuleTo(rule *operationrules.OperationRule) (*model.OperationRule, error) {
	if rule == nil {
		return nil, nil
	}

	ruleDefJSON, err := operationrules.MarshalRuleDefinition(rule.RuleDefinition)
	if err != nil {
		return nil, fmt.Errorf("failed to marshal rule definition: %w", err)
	}

	dbModel := &model.OperationRule{
		ID:             rule.ID,
		Name:           rule.Name,
		OperationType:  string(rule.OperationType),
		OperationCode:  rule.OperationCode,
		RuleDefinition: json.RawMessage(ruleDefJSON),
		IsDefault:      rule.IsDefault,
	}

	if rule.Description != "" {
		dbModel.Description = sql.NullString{String: rule.Description, Valid: true}
	}

	return dbModel, nil
}

// OperationRuleFrom converts database model to domain object
func OperationRuleFrom(dbModel *model.OperationRule) (*operationrules.OperationRule, error) {
	if dbModel == nil {
		return nil, nil
	}

	ruleDef, err := operationrules.UnmarshalRuleDefinition(dbModel.RuleDefinition)
	if err != nil {
		return nil, errors.GRPCErrorInternal(fmt.Sprintf("failed to unmarshal rule definition: %v", err))
	}

	rule := &operationrules.OperationRule{
		ID:             dbModel.ID,
		Name:           dbModel.Name,
		OperationType:  taskcommon.TaskType(dbModel.OperationType),
		OperationCode:  dbModel.OperationCode,
		RuleDefinition: *ruleDef,
		IsDefault:      dbModel.IsDefault,
		CreatedAt:      dbModel.CreatedAt,
		UpdatedAt:      dbModel.UpdatedAt,
	}

	if dbModel.Description.Valid {
		rule.Description = dbModel.Description.String
	}

	return rule, nil
}

// RackRuleAssociationFrom converts database model to domain object
func RackRuleAssociationFrom(dbModel *model.RackRuleAssociation) *operationrules.RackRuleAssociation {
	if dbModel == nil {
		return nil
	}

	return &operationrules.RackRuleAssociation{
		RackID:        dbModel.RackID,
		OperationType: taskcommon.TaskType(dbModel.OperationType),
		OperationCode: dbModel.OperationCode,
		RuleID:        dbModel.RuleID,
		CreatedAt:     dbModel.CreatedAt,
		UpdatedAt:     dbModel.UpdatedAt,
	}
}

// RackRuleAssociationTo converts domain object to database model
func RackRuleAssociationTo(assoc *operationrules.RackRuleAssociation) *model.RackRuleAssociation {
	if assoc == nil {
		return nil
	}

	return &model.RackRuleAssociation{
		RackID:        assoc.RackID,
		OperationType: string(assoc.OperationType),
		OperationCode: assoc.OperationCode,
		RuleID:        assoc.RuleID,
		CreatedAt:     assoc.CreatedAt,
		UpdatedAt:     assoc.UpdatedAt,
	}
}
