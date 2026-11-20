// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"fmt"
	"time"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	validationis "github.com/go-ozzo/ozzo-validation/v4/is"
	"github.com/google/uuid"

	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
)

// APIPowerTaskOperation names a reusable Flow power operation.
type APIPowerTaskOperation string

var powerTaskOperationNames = map[flowv1.PowerControlOperation]APIPowerTaskOperation{
	flowv1.PowerControlOperation_POWER_CONTROL_OPERATION_POWER_ON:        "PowerOn",
	flowv1.PowerControlOperation_POWER_CONTROL_OPERATION_FORCE_POWER_ON:  "ForcePowerOn",
	flowv1.PowerControlOperation_POWER_CONTROL_OPERATION_POWER_OFF:       "PowerOff",
	flowv1.PowerControlOperation_POWER_CONTROL_OPERATION_FORCE_POWER_OFF: "ForcePowerOff",
	flowv1.PowerControlOperation_POWER_CONTROL_OPERATION_RESTART:         "Restart",
	flowv1.PowerControlOperation_POWER_CONTROL_OPERATION_FORCE_RESTART:   "ForceRestart",
	flowv1.PowerControlOperation_POWER_CONTROL_OPERATION_WARM_RESET:      "WarmReset",
	flowv1.PowerControlOperation_POWER_CONTROL_OPERATION_COLD_RESET:      "ColdReset",
}

// FromProto retains known task operations and marks newer values Unknown.
func (o *APIPowerTaskOperation) FromProto(operation flowv1.PowerControlOperation) {
	*o = enumOr(powerTaskOperationNames, operation, "Unknown")
}

// Validate rejects operations that cannot be submitted to Flow.
func (o APIPowerTaskOperation) Validate() error {
	if o.ToProto() == flowv1.PowerControlOperation_POWER_CONTROL_OPERATION_UNSPECIFIED {
		return fmt.Errorf("unsupported power operation %q", o)
	}
	return nil
}

// ToProto converts a validated power operation to Flow's enum.
func (o APIPowerTaskOperation) ToProto() flowv1.PowerControlOperation {
	for value, name := range powerTaskOperationNames {
		if name == o {
			return value
		}
	}
	return flowv1.PowerControlOperation_POWER_CONTROL_OPERATION_UNSPECIFIED
}

// APIFirmwareTaskOperation names a reusable Flow firmware operation.
type APIFirmwareTaskOperation string

var firmwareTaskOperationNames = map[flowv1.FirmwareControlOperation]APIFirmwareTaskOperation{
	flowv1.FirmwareControlOperation_FIRMWARE_CONTROL_OPERATION_UPGRADE:   "Upgrade",
	flowv1.FirmwareControlOperation_FIRMWARE_CONTROL_OPERATION_DOWNGRADE: "Downgrade",
	flowv1.FirmwareControlOperation_FIRMWARE_CONTROL_OPERATION_ROLLBACK:  "Rollback",
}

// Validate rejects operations that cannot be submitted to Flow.
func (o APIFirmwareTaskOperation) Validate() error {
	if o.ToProto() == flowv1.FirmwareControlOperation_FIRMWARE_CONTROL_OPERATION_UNSPECIFIED {
		return fmt.Errorf("unsupported firmware operation %q", o)
	}
	return nil
}

// ToProto converts a validated firmware operation to Flow's enum.
func (o APIFirmwareTaskOperation) ToProto() flowv1.FirmwareControlOperation {
	for value, name := range firmwareTaskOperationNames {
		if name == o {
			return value
		}
	}
	return flowv1.FirmwareControlOperation_FIRMWARE_CONTROL_OPERATION_UNSPECIFIED
}

// FromProto retains known firmware operations and marks newer values Unknown.
func (o *APIFirmwareTaskOperation) FromProto(value flowv1.FirmwareControlOperation) {
	*o = enumOr(firmwareTaskOperationNames, value, "Unknown")
}

// APIPowerControl defines a target-independent power operation.
type APIPowerControl struct {
	Operation              APIPowerTaskOperation `json:"operation"`
	OverrideReadinessCheck bool                  `json:"overrideReadinessCheck"`
}

// Validate checks the REST contract before conversion or side effects.
func (p APIPowerControl) Validate() error {
	return validation.ValidateStruct(&p, validation.Field(&p.Operation, validation.Required))
}

// APIFirmwareControl defines a target-independent firmware operation.
type APIFirmwareControl struct {
	Operation              APIFirmwareTaskOperation `json:"operation"`
	TargetVersion          *string                  `json:"targetVersion"`
	StartTime              *time.Time               `json:"startTime"`
	EndTime                *time.Time               `json:"endTime"`
	SubTargets             []string                 `json:"subTargets"`
	OverrideReadinessCheck bool                     `json:"overrideReadinessCheck"`
	OverrideVersionCheck   bool                     `json:"overrideVersionCheck"`
}

// Validate checks the REST contract before conversion or side effects.
func (f APIFirmwareControl) Validate() error {
	return validation.ValidateStruct(&f,
		validation.Field(&f.Operation, validation.Required),
		validation.Field(&f.StartTime, validation.By(f.validateWindow)),
	)
}
func (f APIFirmwareControl) validateWindow(any) error {
	if (f.StartTime != nil && f.StartTime.Unix() == 0) || (f.EndTime != nil && f.EndTime.Unix() == 0) {
		return fmt.Errorf("execution window cannot contain Unix second zero")
	}
	if f.StartTime != nil && f.EndTime != nil && f.EndTime.Unix() <= f.StartTime.Unix() {
		return fmt.Errorf("endTime must follow startTime in whole seconds")
	}
	return nil
}

// APITaskOperation selects exactly one supported operation.
type APITaskOperation struct {
	// RuleID selects an Operation Rule; nil uses normal rule resolution.
	RuleID          *string             `json:"ruleId"`
	PowerControl    *APIPowerControl    `json:"powerControl"`
	FirmwareControl *APIFirmwareControl `json:"firmwareControl"`
}

// Validate checks the REST contract before conversion or side effects.
func (o APITaskOperation) Validate() error {
	errors := validation.Errors{"ruleId": validation.Validate(o.RuleID, validation.When(o.RuleID != nil, validation.Required), validationis.UUID, validation.NotIn(uuid.Nil.String())), "powerControl": validation.Validate(o.PowerControl), "firmwareControl": validation.Validate(o.FirmwareControl)}
	if (o.PowerControl == nil) == (o.FirmwareControl == nil) {
		errors["operation"] = fmt.Errorf("exactly one powerControl or firmwareControl is required")
	}
	return errors.Filter()
}

// ToProto maps validated REST values to the corresponding Flow request.
func (o APITaskOperation) ToProto() *flowv1.TaskOperation {
	p := &flowv1.TaskOperation{}
	if o.RuleID != nil {
		p.RuleId = &flowv1.UUID{Id: *o.RuleID}
	}
	if o.PowerControl != nil {
		p.Operation = &flowv1.TaskOperation_PowerControl{PowerControl: &flowv1.PowerControlTaskOperation{Operation: o.PowerControl.Operation.ToProto(), OverrideReadinessCheck: o.PowerControl.OverrideReadinessCheck}}
	}
	if o.FirmwareControl != nil {
		f := o.FirmwareControl
		p.Operation = &flowv1.TaskOperation_FirmwareControl{FirmwareControl: &flowv1.FirmwareControlTaskOperation{Operation: f.Operation.ToProto(), TargetVersion: f.TargetVersion, StartTime: timePtrToProto(f.StartTime), EndTime: timePtrToProto(f.EndTime), SubTargets: f.SubTargets, OverrideReadinessCheck: f.OverrideReadinessCheck, OverrideVersionCheck: f.OverrideVersionCheck}}
	}
	return p
}

// FromProto replaces the receiver with the structured Flow representation.
func (o *APITaskOperation) FromProto(p *flowv1.TaskOperation) error {
	*o = APITaskOperation{}
	if p.GetRuleId() != nil {
		id := p.GetRuleId().GetId()
		o.RuleID = &id
	}
	switch {
	case p.GetPowerControl() != nil:
		f := p.GetPowerControl()
		o.PowerControl = &APIPowerControl{OverrideReadinessCheck: f.GetOverrideReadinessCheck()}
		o.PowerControl.Operation.FromProto(f.GetOperation())
	case p.GetFirmwareControl() != nil:
		f := p.GetFirmwareControl()
		o.FirmwareControl = &APIFirmwareControl{TargetVersion: f.TargetVersion, StartTime: timePtrFromProto(f.GetStartTime()), EndTime: timePtrFromProto(f.GetEndTime()), SubTargets: f.GetSubTargets(), OverrideReadinessCheck: f.GetOverrideReadinessCheck(), OverrideVersionCheck: f.GetOverrideVersionCheck()}
		o.FirmwareControl.Operation.FromProto(f.GetOperation())
	default:
		return fmt.Errorf("unsupported Task operation")
	}
	return o.Validate()
}
