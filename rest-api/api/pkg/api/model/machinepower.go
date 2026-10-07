// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"fmt"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	validation "github.com/go-ozzo/ozzo-validation/v4"
)

const (
	MachinePowerActionOn               MachinePowerAction = "On"
	MachinePowerActionGracefulShutdown MachinePowerAction = "GracefulShutdown"
	MachinePowerActionForceOff         MachinePowerAction = "ForceOff"
	MachinePowerActionGracefulRestart  MachinePowerAction = "GracefulRestart"
	MachinePowerActionForceRestart     MachinePowerAction = "ForceRestart"
	MachinePowerActionACPowercycle     MachinePowerAction = "ACPowercycle"
)

type MachinePowerAction string

var validMachinePowerActions = []MachinePowerAction{
	MachinePowerActionOn,
	MachinePowerActionGracefulShutdown,
	MachinePowerActionForceOff,
	MachinePowerActionGracefulRestart,
	MachinePowerActionForceRestart,
	MachinePowerActionACPowercycle,
}

var validMachinePowerActionsAny = func() []interface{} {
	result := make([]interface{}, len(validMachinePowerActions))
	for i, action := range validMachinePowerActions {
		result[i] = action
	}
	return result
}()

type APIMachinePowerControlRequest struct {
	// Action is the power control action to perform on the machine
	Action MachinePowerAction `json:"action"`
	// AcknowledgeAttachedInstance is a boolean to indicate caller is aware that an Instance is currently attached to the machine
	AcknowledgeAttachedInstance *bool `json:"acknowledgeAttachedInstance"`
}

func (r *APIMachinePowerControlRequest) Validate() error {
	return validation.ValidateStruct(r,
		validation.Field(&r.Action,
			validation.Required.Error(validationErrorValueRequired),
			validation.In(validMachinePowerActionsAny...).Error(fmt.Sprintf("must be one of %v", validMachinePowerActions))),
	)
}

func (r *APIMachinePowerControlRequest) ToProto(machineID string) *corev1.AdminPowerControlRequest {
	return &corev1.AdminPowerControlRequest{
		MachineId: &machineID,
		Action:    r.Action.ToProto(),
	}
}

func (action MachinePowerAction) ToProto() corev1.AdminPowerControlRequest_SystemPowerControl {
	switch action {
	case MachinePowerActionGracefulShutdown:
		return corev1.AdminPowerControlRequest_GracefulShutdown
	case MachinePowerActionForceOff:
		return corev1.AdminPowerControlRequest_ForceOff
	case MachinePowerActionGracefulRestart:
		return corev1.AdminPowerControlRequest_GracefulRestart
	case MachinePowerActionForceRestart:
		return corev1.AdminPowerControlRequest_ForceRestart
	case MachinePowerActionACPowercycle:
		return corev1.AdminPowerControlRequest_ACPowercycle
	default:
		return corev1.AdminPowerControlRequest_On
	}
}
